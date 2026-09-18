//! `/v1` logs domain: frontend log ingest into the backend's structured
//! logging.

use axum::Json;
use axum::Router;
use axum::routing::post;
use serde_json::Value;

use crate::ServerState;
use hya_api::v1 as pb;

pub(crate) fn router() -> Router<ServerState> {
    Router::new().route("/v1/logs", post(ingest_log))
}

async fn ingest_log(Json(request): Json<pb::IngestLogRequest>) -> Json<pb::IngestLogResponse> {
    let extra: Value = request
        .extra
        .map(|extra| super::convert::from_struct(&extra))
        .unwrap_or_else(|| Value::Object(Default::default()));
    let message = &request.message;
    let service = &request.service;
    match pb::LogLevel::try_from(request.level).unwrap_or(pb::LogLevel::Unspecified) {
        pb::LogLevel::Debug => {
            tracing::debug!(service = %service, extra = ?extra, "{message}");
        }
        pb::LogLevel::Warn => {
            tracing::warn!(service = %service, extra = ?extra, "{message}");
        }
        pb::LogLevel::Error => {
            tracing::error!(service = %service, extra = ?extra, "{message}");
        }
        pb::LogLevel::Info | pb::LogLevel::Unspecified => {
            tracing::info!(service = %service, extra = ?extra, "{message}");
        }
    }
    Json(pb::IngestLogResponse {})
}
