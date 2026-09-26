//! `/v1` process domain: health, location, config, dispose/upgrade, and
//! the aggregated bootstrap snapshot.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;

use super::Json;
use serde_json::Value;

use crate::ServerState;
use hya_api::v1 as pb;

use super::{V1Error, request_scope};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/location", get(location))
        .route(
            "/v1/config",
            get(get_config).patch(axum::routing::patch(update_config)),
        )
        .route("/v1/process/dispose", axum::routing::post(dispose))
        .route("/v1/process/upgrade", axum::routing::post(upgrade))
        .route("/v1/bootstrap", get(bootstrap))
}

async fn health() -> Json<pb::GetHealthResponse> {
    Json(pb::GetHealthResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

async fn location(headers: HeaderMap) -> Result<Json<pb::LocationInfo>, V1Error> {
    let scope = request_scope(&headers, "")?;
    Ok(Json(location_info(scope.as_deref())))
}

/// The backend has no working directory of its own (ADR-0024): `directory`
/// echoes the request's scope, empty when it named none.
fn location_info(scope: Option<&std::path::Path>) -> pb::LocationInfo {
    pb::LocationInfo {
        directory: scope
            .map(|scope| scope.to_string_lossy().into_owned())
            .unwrap_or_default(),
        hostname: hostname(),
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn hostname() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most `buffer.len()` bytes into the
    // provided buffer and the return value is checked before reading.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return "unknown".to_owned();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

async fn get_config(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::GetConfigResponse>, V1Error> {
    let _request: pb::GetConfigRequest = super::query_request(&[], &query)?;
    Ok(Json(pb::GetConfigResponse {
        values: Some(super::convert::to_struct(st.global.config().await)),
    }))
}

async fn update_config(
    State(st): State<ServerState>,
    Json(request): Json<pb::UpdateConfigRequest>,
) -> Result<Json<pb::GetConfigResponse>, V1Error> {
    let patch = request
        .patch
        .as_ref()
        .map(super::convert::from_struct)
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !patch.is_object() {
        return Err(V1Error::invalid_argument("config patch must be an object"));
    }
    let current = st.global.config().await;
    let merged = merge_objects(current, patch);
    st.global.update_config(merged.clone()).await;
    Ok(Json(pb::GetConfigResponse {
        values: Some(super::convert::to_struct(merged)),
    }))
}

/// Deep-merge `patch` into `base`; non-object leaves in `patch` replace.
fn merge_objects(base: Value, patch: Value) -> Value {
    match (base, patch) {
        (Value::Object(mut base), Value::Object(patch)) => {
            for (key, value) in patch {
                let merged = match base.remove(&key) {
                    Some(existing) => merge_objects(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (_, patch) => patch,
    }
}

async fn dispose() -> Result<Json<pb::DisposeProcessResponse>, V1Error> {
    Err(V1Error::unavailable(
        "process disposal is owned by the host process supervisor; HTTP dispose is not wired",
    ))
}

async fn upgrade() -> Result<Json<pb::UpgradeProcessResponse>, V1Error> {
    Err(V1Error::unavailable(
        "self-update runs through the verified launcher updater; HTTP upgrade is not wired",
    ))
}

async fn bootstrap(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::Bootstrap>, V1Error> {
    let request: pb::GetBootstrapRequest = super::query_request(&[], &query)?;
    // Without a scope the catalog rows are the global (project-less) view.
    let workdir = request_scope(&headers, &request.directory)?;

    let agents = super::catalog::agent_rows(&st, workdir.as_deref()).await?;
    let models = super::catalog::model_rows(&st);
    let providers = super::catalog::provider_rows(&st, &models).await;
    let commands = super::catalog::command_rows(workdir.as_deref());
    let skills = super::catalog::skill_rows(workdir.as_deref());
    let tools = super::catalog::tool_rows(&st);

    Ok(Json(pb::Bootstrap {
        location: Some(location_info(workdir.as_deref())),
        config: Some(super::convert::to_struct(st.global.config().await)),
        agents,
        models: models.clone(),
        providers,
        commands,
        skills,
        tools,
        interactions: Vec::new(),
        saved_rules: super::catalog::saved_rule_rows(&st).await,
        formatter_available: !st.formatter_status.is_empty(),
        sessions_cursor: String::new(),
    }))
}
