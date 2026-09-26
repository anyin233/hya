//! `/v1` auth domain: provider credential storage.
//!
//! Keys are written by the app-owned [`crate::ProviderControl`] through
//! `hya_app::auth` (atomic, mode 0600, `type: api`), which then rebuilds the
//! provider's route and the catalog live — no restart. Provider OAuth flows
//! over HTTP are not wired for v1 yet and answer `unavailable` honestly
//! rather than faking a flow.

use axum::extract::{Path as AxumPath, State};
use axum::routing::{get, put};
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;

use super::V1Error;
use super::providers::{check_provider_id, discovery_outcome, map_control_error};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/auth", get(list_provider_auth))
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

async fn list_provider_auth(
    State(st): State<ServerState>,
) -> Result<Json<pb::ListProviderAuthResponse>, V1Error> {
    let provider_ids = st
        .provider_control
        .list_saved_keys()
        .await
        .map_err(map_control_error)?;
    Ok(Json(pb::ListProviderAuthResponse { provider_ids }))
}

async fn set_provider_auth(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Json(request): Json<pb::SetProviderAuthRequest>,
) -> Result<Json<pb::SetProviderAuthResponse>, V1Error> {
    check_provider_id(&provider_id)?;
    let token = match &request.secret {
        Some(pb::set_provider_auth_request::Secret::ApiKey(key)) => key.clone(),
        Some(pb::set_provider_auth_request::Secret::Oauth(oauth)) => oauth.access_token.clone(),
        None => return Err(V1Error::invalid_argument("missing credential payload")),
    };
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(V1Error::invalid_argument("credential payload is empty"));
    }
    if token.chars().any(char::is_control) {
        return Err(V1Error::invalid_argument(
            "credential payload contains control characters",
        ));
    }
    let change = st
        .provider_control
        .set_key(provider_id.clone(), token)
        .await
        .map_err(map_control_error)?;
    super::providers::notify_catalog_updated(&st);
    Ok(Json(pb::SetProviderAuthResponse {
        status: pb::AuthStatus::Credentialed as i32,
        provider: if change.configured {
            super::catalog::provider_info(&st, &provider_id).await
        } else {
            None
        },
        discovery: change.discovery.as_ref().map(discovery_outcome),
    }))
}

async fn remove_provider_auth(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
) -> Result<Json<pb::RemoveProviderAuthResponse>, V1Error> {
    check_provider_id(&provider_id)?;
    let (_removed, change) = st
        .provider_control
        .remove_key(provider_id.clone())
        .await
        .map_err(map_control_error)?;
    super::providers::notify_catalog_updated(&st);
    Ok(Json(pb::RemoveProviderAuthResponse {
        provider: if change.configured {
            super::catalog::provider_info(&st, &provider_id).await
        } else {
            None
        },
    }))
}

async fn start_oauth(
    AxumPath(provider_id): AxumPath<String>,
    Json(_request): Json<pb::StartOauthRequest>,
) -> Result<Json<pb::StartOauthResponse>, V1Error> {
    check_provider_id(&provider_id)?;
    Err(V1Error::unavailable(format!(
        "provider oauth start is not wired for {provider_id}; use the launcher auth command"
    )))
}

async fn complete_oauth(
    AxumPath(provider_id): AxumPath<String>,
    Json(_request): Json<pb::CompleteOauthRequest>,
) -> Result<Json<pb::CompleteOauthResponse>, V1Error> {
    check_provider_id(&provider_id)?;
    Err(V1Error::unavailable(format!(
        "provider oauth completion is not wired for {provider_id}; use the launcher auth command"
    )))
}
