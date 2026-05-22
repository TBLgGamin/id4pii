use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt;
use hudsucker::hyper::{Method, Request, Response, header};
use hudsucker::{Body, HttpContext, HttpHandler, RequestOrResponse, decode_response};
use id4pii_core::{Detector, Rng, SseDeanonymizer, Vault, anonymize_json, deanonymize_json};
use serde_json::Value;
use tracing::{info, warn};

use super::hosts::HostMatcher;

#[derive(Clone)]
pub(crate) struct PiiHandler {
    detector: Arc<Mutex<Detector>>,
    vault: Arc<Mutex<Vault>>,
    rng: Arc<Mutex<Rng>>,
    hosts: Arc<HostMatcher>,
}

impl PiiHandler {
    pub(crate) fn new(
        detector: Arc<Mutex<Detector>>,
        vault: Arc<Mutex<Vault>>,
        rng: Arc<Mutex<Rng>>,
        hosts: Arc<HostMatcher>,
    ) -> Self {
        Self {
            detector,
            vault,
            rng,
            hosts,
        }
    }

    async fn anonymize_bytes(&self, bytes: Bytes) -> (Bytes, usize) {
        let mut value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return (bytes, 0),
        };
        let detector = self.detector.clone();
        let vault = self.vault.clone();
        let rng = self.rng.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut detector = detector
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut vault = vault
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut rng = rng
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let count = anonymize_json(&mut value, &mut detector, &mut rng, &mut vault);
            (value, count)
        })
        .await;
        match task {
            Ok((value, count)) => match serde_json::to_vec(&value) {
                Ok(encoded) => (Bytes::from(encoded), count),
                Err(_) => (bytes, 0),
            },
            Err(_) => (bytes, 0),
        }
    }

    fn vault_snapshot(&self) -> Vault {
        self.vault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl HttpHandler for PiiHandler {
    async fn should_intercept(&mut self, _ctx: &HttpContext, req: &Request<Body>) -> bool {
        req.uri()
            .host()
            .is_some_and(|host| self.hosts.matches(host))
    }

    async fn handle_request(
        &mut self,
        _ctx: &HttpContext,
        req: Request<Body>,
    ) -> RequestOrResponse {
        if req.method() != Method::POST || !is_json(req.headers()) {
            return req.into();
        }
        let uri = req.uri().to_string();
        let (mut parts, body) = req.into_parts();
        let collected = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) => {
                warn!("failed to read request body: {error}");
                return Request::from_parts(parts, Body::empty()).into();
            }
        };
        let (anonymized, count) = self.anonymize_bytes(collected).await;
        info!("request to {uri}: anonymized {count} field(s) containing PII");
        set_content_length(&mut parts.headers, anonymized.len());
        Request::from_parts(parts, Body::from(anonymized)).into()
    }

    async fn handle_response(&mut self, _ctx: &HttpContext, res: Response<Body>) -> Response<Body> {
        let res = match decode_response(res) {
            Ok(res) => res,
            Err(error) => {
                warn!("failed to decode response: {error}");
                return Response::new(Body::empty());
            }
        };
        let content_type = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        if content_type.contains("event-stream") {
            info!("de-anonymizing streaming response");
            let deanon = SseDeanonymizer::new(&self.vault_snapshot());
            let (mut parts, body) = res.into_parts();
            parts.headers.remove(header::CONTENT_LENGTH);
            let stream = futures::stream::unfold(
                (body.into_data_stream(), deanon, false),
                |(mut stream, mut deanon, finished)| async move {
                    if finished {
                        return None;
                    }
                    let (out, done) = match stream.next().await {
                        Some(Ok(chunk)) => (deanon.push(&chunk), false),
                        _ => (deanon.finish(), true),
                    };
                    let item: std::result::Result<Bytes, std::io::Error> = Ok(Bytes::from(out));
                    Some((item, (stream, deanon, done)))
                },
            );
            return Response::from_parts(parts, Body::from_stream(stream));
        }

        if content_type.contains("json") {
            let (mut parts, body) = res.into_parts();
            let collected = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(error) => {
                    warn!("failed to read response body: {error}");
                    return Response::new(Body::empty());
                }
            };
            let (restored, count) = self.deanonymize_bytes(&collected);
            if count > 0 {
                info!("response: restored {count} field(s) to real values");
            }
            set_content_length(&mut parts.headers, restored.len());
            return Response::from_parts(parts, Body::from(restored));
        }

        res
    }
}

impl PiiHandler {
    fn deanonymize_bytes(&self, bytes: &Bytes) -> (Bytes, usize) {
        let mut value: Value = match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(_) => return (bytes.clone(), 0),
        };
        let count = deanonymize_json(&mut value, &self.vault_snapshot());
        match serde_json::to_vec(&value) {
            Ok(encoded) => (Bytes::from(encoded), count),
            Err(_) => (bytes.clone(), 0),
        }
    }
}

fn is_json(headers: &header::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("json"))
}

fn set_content_length(headers: &mut header::HeaderMap, length: usize) {
    headers.remove(header::CONTENT_LENGTH);
    if let Ok(value) = header::HeaderValue::from_str(&length.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
}
