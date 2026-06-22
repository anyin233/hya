use std::collections::BTreeMap;

use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::ServerState;

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/log", post(log_entry))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogInput {
    service: String,
    level: LogLevel,
    message: String,
    extra: Option<BTreeMap<String, Value>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum LogLevel {
    Debug,
    Info,
    Error,
    Warn,
}

async fn log_entry(Json(input): Json<LogInput>) -> Json<bool> {
    let extra = input.extra.unwrap_or_default();
    match input.level {
        LogLevel::Debug => tracing::debug!(
            service = %input.service,
            extra = ?extra,
            "{}",
            input.message
        ),
        LogLevel::Info => tracing::info!(
            service = %input.service,
            extra = ?extra,
            "{}",
            input.message
        ),
        LogLevel::Warn => tracing::warn!(
            service = %input.service,
            extra = ?extra,
            "{}",
            input.message
        ),
        LogLevel::Error => tracing::error!(
            service = %input.service,
            extra = ?extra,
            "{}",
            input.message
        ),
    }
    Json(true)
}
