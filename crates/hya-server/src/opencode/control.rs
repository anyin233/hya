use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::extract::Path as AxumPath;
use axum::routing::post;
use axum::routing::put;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::{ApiError, ServerState};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/auth/:provider_id", put(auth_set).delete(auth_remove))
        .route("/log", post(log_entry))
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum AuthInfo {
    Api {
        key: String,
        #[serde(default, rename = "metadata")]
        _metadata: Option<BTreeMap<String, String>>,
    },
    Oauth {
        #[serde(rename = "refresh")]
        _refresh: String,
        access: String,
        #[serde(rename = "expires")]
        _expires: u64,
        #[serde(default, rename = "accountId")]
        _account_id: Option<String>,
        #[serde(default, rename = "enterpriseUrl")]
        _enterprise_url: Option<String>,
    },
    Wellknown {
        #[serde(rename = "key")]
        _key: String,
        token: String,
    },
}

impl AuthInfo {
    fn token(&self) -> &str {
        match self {
            Self::Api { key, .. } => key,
            Self::Oauth { access, .. } => access,
            Self::Wellknown { token, .. } => token,
        }
    }
}

async fn auth_set(
    AxumPath(provider_id): AxumPath<String>,
    Json(input): Json<AuthInfo>,
) -> Result<Json<bool>, ApiError> {
    validate_provider_id(&provider_id)?;
    let dir = auth_dir().ok_or_else(|| ApiError::internal("no config directory"))?;
    save_token_in(&dir, &provider_id, input.token())
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(true))
}

async fn auth_remove(AxumPath(provider_id): AxumPath<String>) -> Result<Json<bool>, ApiError> {
    validate_provider_id(&provider_id)?;
    let dir = auth_dir().ok_or_else(|| ApiError::internal("no config directory"))?;
    let path = dir.join(format!("{provider_id}.yaml"));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(Json(true)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Json(true)),
        Err(error) => Err(ApiError::internal(error.to_string())),
    }
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

fn validate_provider_id(provider_id: &str) -> Result<(), ApiError> {
    if provider_id.is_empty()
        || provider_id.contains('/')
        || provider_id.contains('\\')
        || provider_id.contains("..")
    {
        return Err(ApiError::bad_request("invalid provider id"));
    }
    Ok(())
}

fn auth_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("hya/auth"))
}

fn save_token_in(dir: &Path, provider: &str, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let body = format!("token: \"{}\"\n", yaml_escape(token.trim()));
    std::fs::write(dir.join(format!("{provider}.yaml")), body)
}

fn yaml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}
