//! `/v1` auth domain: provider credential storage.
//!
//! Keys are written by the app-owned [`crate::ProviderControl`] through
//! `hya_app::auth` (atomic, mode 0600, `type: api`), which then rebuilds the
//! provider's route and the catalog live — no restart. Provider OAuth flows
//! use the app-owned control for official Sign in with ChatGPT.

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::routing::{get, put};

use super::Json;

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
        warning: change.warning.unwrap_or_default(),
        provider: if change.configured {
            super::catalog::provider_info(&st, &provider_id).await
        } else {
            None
        },
    }))
}

async fn start_oauth(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Json(_request): Json<pb::StartOauthRequest>,
) -> Result<Json<pb::StartOauthResponse>, V1Error> {
    check_provider_id(&provider_id)?;
    let started = st
        .provider_control
        .start_oauth(provider_id)
        .await
        .map_err(map_control_error)?;
    Ok(Json(pb::StartOauthResponse {
        authorization_url: started.authorization_url,
        state: started.state,
    }))
}

async fn complete_oauth(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Json(request): Json<pb::CompleteOauthRequest>,
) -> Result<Json<pb::CompleteOauthResponse>, V1Error> {
    check_provider_id(&provider_id)?;
    st.provider_control
        .complete_oauth(provider_id, request.state, request.code)
        .await
        .map_err(map_control_error)?;
    super::providers::notify_catalog_updated(&st);
    Ok(Json(pb::CompleteOauthResponse {
        status: pb::AuthStatus::Credentialed as i32,
    }))
}
