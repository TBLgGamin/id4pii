use std::path::PathBuf;

use crate::{Detector, PiiSpan, RedactStyle, Rng, Vault, anonymize, deanonymize, redact};
use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::info;

const MAX_REQUEST_BATCH: usize = 16;

type DetectResult = std::result::Result<Vec<PiiSpan>, String>;

struct DetectJob {
    text: String,
    reply: oneshot::Sender<DetectResult>,
}

#[derive(Clone)]
struct AppState {
    jobs: mpsc::UnboundedSender<DetectJob>,
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

fn spawn_batcher(mut detector: Detector, mut rx: mpsc::UnboundedReceiver<DetectJob>) -> Result<()> {
    std::thread::Builder::new()
        .name("id4pii-detect-batcher".to_string())
        .spawn(move || {
            while let Some(first) = rx.blocking_recv() {
                let mut jobs = vec![first];
                while jobs.len() < MAX_REQUEST_BATCH {
                    match rx.try_recv() {
                        Ok(job) => jobs.push(job),
                        Err(_) => break,
                    }
                }
                let texts: Vec<&str> = jobs.iter().map(|job| job.text.as_str()).collect();
                let outcome = detector.detect_batch(&texts, 0.0);
                drop(texts);
                match outcome {
                    Ok(results) => {
                        for (job, spans) in jobs.into_iter().zip(results) {
                            let _ = job.reply.send(Ok(spans));
                        }
                    }
                    Err(err) => {
                        let message = err.to_string();
                        for job in jobs {
                            let _ = job.reply.send(Err(message.clone()));
                        }
                    }
                }
            }
        })
        .context("failed to spawn detect batcher thread")?;
    Ok(())
}

async fn detect(state: &AppState, text: String) -> std::result::Result<Vec<PiiSpan>, ApiError> {
    let (reply, response) = oneshot::channel();
    state
        .jobs
        .send(DetectJob { text, reply })
        .map_err(|_| ApiError("detector unavailable".to_string()))?;
    response
        .await
        .map_err(|_| ApiError("detector dropped the request".to_string()))?
        .map_err(ApiError)
}

pub(crate) async fn run(
    addr: String,
    model: PathBuf,
    model_file: String,
    threads: usize,
    min_score: f32,
) -> Result<()> {
    crate::model_setup::ensure_model(&model, &model_file)?;
    let detector = Detector::load(&model, &model_file, threads).context("failed to load model")?;
    let (jobs, rx) = mpsc::unbounded_channel();
    spawn_batcher(detector, rx)?;
    let state = AppState { jobs, min_score };

    let app = Router::new()
        .route("/health", get(health))
        .route("/scan", post(scan))
        .route("/anonymize", post(anonymize_route))
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

async fn deanonymize_route(Json(request): Json<DeanonymizeRequest>) -> Json<DeanonymizeResponse> {
    Json(DeanonymizeResponse {
        text: deanonymize(&request.text, &request.vault),
    })
}
