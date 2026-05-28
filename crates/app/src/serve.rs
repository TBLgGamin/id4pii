use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use id4pii_core::{Detector, PiiSpan, RedactStyle, Rng, Vault, anonymize, deanonymize, redact};
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Clone)]
struct AppState {
    detector: Arc<Mutex<Detector>>,
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

pub(crate) async fn run(
    addr: String,
    model: PathBuf,
    model_file: String,
    threads: usize,
    min_score: f32,
) -> Result<()> {
    crate::model_setup::ensure_model(&model, &model_file)?;
    let detector = Detector::load(&model, &model_file, threads).context("failed to load model")?;
    let state = AppState {
        detector: Arc::new(Mutex::new(detector)),
        min_score,
    };

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
    let response =
        tokio::task::spawn_blocking(move || -> std::result::Result<ScanResponse, String> {
            let mut detector = state
                .detector
                .lock()
                .map_err(|_| "detector lock poisoned".to_string())?;
            let threshold = request.min_score.unwrap_or(state.min_score);
            let spans = detector
                .detect(&request.text, threshold)
                .map_err(|e| e.to_string())?;
            let redacted = request
                .redact
                .then(|| redact(&request.text, &spans, RedactStyle::Label));
            Ok(ScanResponse { spans, redacted })
        })
        .await
        .map_err(|e| ApiError(e.to_string()))?
        .map_err(ApiError)?;

    Ok(Json(response))
}

async fn anonymize_route(
    State(state): State<AppState>,
    Json(request): Json<AnonymizeRequest>,
) -> std::result::Result<Json<AnonymizeResponse>, ApiError> {
    let response =
        tokio::task::spawn_blocking(move || -> std::result::Result<AnonymizeResponse, String> {
            let mut detector = state
                .detector
                .lock()
                .map_err(|_| "detector lock poisoned".to_string())?;
            let threshold = request.min_score.unwrap_or(state.min_score);
            let spans = detector
                .detect(&request.text, threshold)
                .map_err(|e| e.to_string())?;
            let mut rng = request.seed.map_or_else(Rng::from_entropy, Rng::new);
            let (anonymized, vault) = anonymize(&request.text, &spans, &mut rng);
            Ok(AnonymizeResponse { anonymized, vault })
        })
        .await
        .map_err(|e| ApiError(e.to_string()))?
        .map_err(ApiError)?;

    Ok(Json(response))
}

async fn deanonymize_route(Json(request): Json<DeanonymizeRequest>) -> Json<DeanonymizeResponse> {
    Json(DeanonymizeResponse {
        text: deanonymize(&request.text, &request.vault),
    })
}
