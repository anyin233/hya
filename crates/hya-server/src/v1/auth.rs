//! `/v1` auth domain: provider credential storage.
//!
//! API keys are stored per provider in the user auth directory, mirroring
//! the legacy `PUT /auth/:provider_id` surface. Provider OAuth flows over
//! HTTP are not wired for v1 yet and answer `unavailable` honestly rather
//! than faking a flow.

use std::path::PathBuf;

use axum::extract::{Path as AxumPath, State};
use axum::routing::put;
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;

use super::V1Error;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route(
            "/v1/auth/:provider_id",
            put(set_provider_auth).delete(remove_provider_auth),
        )
        .route(
            "/v1/auth/:provider_id/oauth/start",
            axum::routing::post(start_oauth),
        )
        .route(
            "/v1/auth/:provider_id/oauth/callback",
            axum::routing::post(complete_oauth),
        )
}

async fn set_provider_auth(
    State(_st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Json(request): Json<pb::SetProviderAuthRequest>,
) -> Result<Json<pb::SetProviderAuthResponse>, V1Error> {
    validate_provider_id(&provider_id)?;
    let token = match &request.secret {
        Some(pb::set_provider_auth_request::Secret::ApiKey(key)) => key.clone(),
        Some(pb::set_provider_auth_request::Secret::Oauth(oauth)) => oauth.access_token.clone(),
        None => return Err(V1Error::invalid_argument("missing credential payload")),
    };
    if token.trim().is_empty() {
        return Err(V1Error::invalid_argument("credential payload is empty"));
    }
    let dir = auth_dir().ok_or_else(|| V1Error::internal("no config directory"))?;
    save_token_in(&dir, &provider_id, &token)
        .map_err(|error| V1Error::internal(error.to_string()))?;
    Ok(Json(pb::SetProviderAuthResponse {
        status: pb::AuthStatus::Credentialed as i32,
    }))
}

async fn remove_provider_auth(
    State(_st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
) -> Result<Json<pb::RemoveProviderAuthResponse>, V1Error> {
    validate_provider_id(&provider_id)?;
    let dir = auth_dir().ok_or_else(|| V1Error::internal("no config directory"))?;
    let path = dir.join(format!("{provider_id}.yaml"));
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(V1Error::internal(error.to_string())),
    }
    Ok(Json(pb::RemoveProviderAuthResponse {}))
}

async fn start_oauth(
    AxumPath(provider_id): AxumPath<String>,
    Json(_request): Json<pb::StartOauthRequest>,
) -> Result<Json<pb::StartOauthResponse>, V1Error> {
    validate_provider_id(&provider_id)?;
    Err(V1Error::unavailable(format!(
        "provider oauth start is not wired for {provider_id}; use the launcher auth command"
    )))
}

async fn complete_oauth(
    AxumPath(provider_id): AxumPath<String>,
    Json(_request): Json<pb::CompleteOauthRequest>,
) -> Result<Json<pb::CompleteOauthResponse>, V1Error> {
    validate_provider_id(&provider_id)?;
    Err(V1Error::unavailable(format!(
        "provider oauth completion is not wired for {provider_id}; use the launcher auth command"
    )))
}

fn validate_provider_id(provider_id: &str) -> Result<(), V1Error> {
    let valid = provider_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !provider_id.is_empty()
        && !provider_id.contains("..");
    if valid {
        Ok(())
    } else {
        Err(V1Error::invalid_argument("invalid provider id"))
    }
}

fn auth_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("hya/auth"))
}

fn save_token_in(dir: &std::path::Path, provider: &str, token: &str) -> std::io::Result<()> {
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
