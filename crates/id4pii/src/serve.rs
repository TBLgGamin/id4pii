use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::info;

use base64::{Engine as _, prelude::BASE64_STANDARD};

use crate::detector_service::{Coalesce, DetectorService};
use crate::{PiiSpan, RedactStyle, Rng, Vault, anonymize, deanonymize, redact};

const MAX_REQUEST_BATCH: usize = 16;
const QUEUE_DEPTH: usize = 256;

#[derive(Clone)]
struct AppState {
    service: DetectorService,
    min_score: f32,
}

#[derive(Deserialize)]
struct ScanRequest {
    text: String,
    #[serde(default)]
    redact: bool,
    #[serde(default)]
    min_score: Option<f32>,
}

#[derive(Serialize)]
struct ScanResponse {
    spans: Vec<PiiSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    redacted: Option<String>,
}

#[derive(Deserialize)]
struct AnonymizeRequest {
    text: String,
    #[serde(default)]
    seed: Option<u64>,
    #[serde(default)]
    min_score: Option<f32>,
}

#[derive(Serialize)]
struct AnonymizeResponse {
    anonymized: String,
    vault: Vault,
}

#[derive(Deserialize)]
struct AnonymizeFileRequest {
    filename: String,
    data: String,
    #[serde(default)]
    seed: Option<u64>,
    #[serde(default)]
    min_score: Option<f32>,
}

#[derive(Serialize)]
struct AnonymizeFileResponse {
    data: String,
    mime: &'static str,
    count: usize,
    vault: Vault,
}

#[derive(Deserialize)]
struct DeanonymizeRequest {
    text: String,
    vault: Vault,
}

#[derive(Serialize)]
struct DeanonymizeResponse {
    text: String,
}

struct ApiError(String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0).into_response()
    }
}

async fn detect(state: &AppState, text: String) -> std::result::Result<Vec<PiiSpan>, ApiError> {
    let service = state.service.clone();
    let mut batches = tokio::task::spawn_blocking(move || service.submit(vec![text], 0.0))
        .await
        .map_err(|_| ApiError("detector task panicked".to_string()))?
        .map_err(ApiError)?;
    Ok(batches.pop().unwrap_or_default())
}

pub(crate) async fn run(
    addr: String,
    model: PathBuf,
    model_file: String,
    threads: usize,
    min_score: f32,
) -> Result<()> {
    let detector = crate::model_setup::load_detector(&model, &model_file, threads)?;
    let (service, _handle) =
        DetectorService::spawn(detector, Coalesce::UpTo(MAX_REQUEST_BATCH), QUEUE_DEPTH)?;
    let state = AppState { service, min_score };

    let app = Router::new()
        .route("/health", get(health))
        .route("/scan", post(scan))
        .route("/anonymize", post(anonymize_route))
        .route("/anonymize-file", post(anonymize_file_route))
        .route("/deanonymize", post(deanonymize_route))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    info!("id4pii api listening on http://{addr}");
    axum::serve(listener, app).await.context("server error")?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn scan(
    State(state): State<AppState>,
    Json(request): Json<ScanRequest>,
) -> std::result::Result<Json<ScanResponse>, ApiError> {
    let threshold = request.min_score.unwrap_or(state.min_score);
    let mut spans = detect(&state, request.text.clone()).await?;
    if threshold > 0.0 {
        spans.retain(|span| span.score >= threshold);
    }
    let redacted = request
        .redact
        .then(|| redact(&request.text, &spans, RedactStyle::Label));
    Ok(Json(ScanResponse { spans, redacted }))
}

async fn anonymize_route(
    State(state): State<AppState>,
    Json(request): Json<AnonymizeRequest>,
) -> std::result::Result<Json<AnonymizeResponse>, ApiError> {
    let threshold = request.min_score.unwrap_or(state.min_score);
    let mut spans = detect(&state, request.text.clone()).await?;
    if threshold > 0.0 {
        spans.retain(|span| span.score >= threshold);
    }
    let mut rng = request.seed.map_or_else(Rng::from_entropy, Rng::new);
    let (anonymized, vault) = anonymize(&request.text, &spans, &mut rng);
    Ok(Json(AnonymizeResponse { anonymized, vault }))
}

async fn anonymize_file_route(
    State(state): State<AppState>,
    Json(request): Json<AnonymizeFileRequest>,
) -> std::result::Result<Json<AnonymizeFileResponse>, ApiError> {
    let service = state.service.clone();
    let threshold = request.min_score.unwrap_or(state.min_score);
    let response = tokio::task::spawn_blocking(
        move || -> std::result::Result<AnonymizeFileResponse, String> {
            let bytes = BASE64_STANDARD
                .decode(request.data.as_bytes())
                .map_err(|e| format!("base64 decode failed: {e}"))?;
            let mut rng = request.seed.map_or_else(Rng::from_entropy, Rng::new);
            let mut vault = Vault::default();

            let (output, count) = crate::document::anonymize_document(
                &bytes,
                &request.filename,
                |text| {
                    let mut spans = service
                        .submit(vec![text.to_string()], 0.0)
                        .map_err(|e| anyhow::anyhow!(e))?
                        .pop()
                        .unwrap_or_default();
                    if threshold > 0.0 {
                        spans.retain(|span| span.score >= threshold);
                    }
                    Ok(spans)
                },
                &mut rng,
                &mut vault,
            )
            .map_err(|e| e.to_string())?;
            Ok(AnonymizeFileResponse {
                data: BASE64_STANDARD.encode(&output.data),
                mime: output.mime,
                count,
                vault,
            })
        },
    )
    .await
    .map_err(|_| ApiError("anonymize-file task panicked".to_string()))?;
    response.map(Json).map_err(ApiError)
}

async fn deanonymize_route(Json(request): Json<DeanonymizeRequest>) -> Json<DeanonymizeResponse> {
    Json(DeanonymizeResponse {
        text: deanonymize(&request.text, &request.vault),
    })
}
