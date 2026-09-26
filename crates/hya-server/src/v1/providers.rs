//! `/v1` provider management: upsert a provider, refresh its remote model
//! list, write or remove config model overrides, and test a model.
//!
//! Mutations go through the app-owned [`crate::ProviderControl`], which
//! writes `config.yaml` / the auth directory / the model cache, rebuilds the
//! provider's route and the catalog, and publishes them on the engine before
//! returning. The handler then answers from the live catalog and emits
//! `catalog.updated`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use hya_api::v1 as pb;
use hya_proto::{FinishReason, ModelRef};
use hya_provider::{ProviderError, ProviderKind};

use crate::ServerState;
use crate::provider_control::{
    PROVIDER_CONTROL_UNAVAILABLE, PROVIDER_INVALID_REQUEST, PROVIDER_NOT_FOUND, ProviderChange,
    ProviderControlError, ProviderDiscoveryReport, ProviderModelOverride, ProviderUpsert,
    valid_provider_id,
};

use super::V1Error;

/// Wall-clock bound for one model probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for one probe error message.
const PROBE_MESSAGE_LIMIT: usize = 512;

/// Map a control failure onto the stable v1 error model.
pub(crate) fn map_control_error(error: ProviderControlError) -> V1Error {
    match error.code.as_str() {
        PROVIDER_INVALID_REQUEST => V1Error::invalid_argument(error.message),
        PROVIDER_NOT_FOUND => V1Error::new(hya_api::error::Code::NotFound, error.message),
        PROVIDER_CONTROL_UNAVAILABLE => V1Error::unavailable(error.message),
        _ => V1Error::internal(error.message),
    }
}

/// Reject an id that cannot name a provider.
pub(crate) fn check_provider_id(provider_id: &str) -> Result<(), V1Error> {
    if valid_provider_id(provider_id) {
        Ok(())
    } else {
        Err(V1Error::invalid_argument(
            "invalid provider id: use 1-64 ASCII letters, digits, '-' or '_'",
        ))
    }
}

fn check_model_id(model_id: &str) -> Result<String, V1Error> {
    let model_id = model_id.trim();
    if model_id.is_empty() || model_id.len() > 256 || model_id.chars().any(char::is_control) {
        return Err(V1Error::invalid_argument(
            "invalid model id: 1-256 characters without control characters",
        ));
    }
    Ok(model_id.to_owned())
}

pub(crate) fn discovery_outcome(report: &ProviderDiscoveryReport) -> pb::DiscoveryOutcome {
    pb::DiscoveryOutcome {
        ok: report.ok,
        result: report.result.clone(),
        error_message: report.error_message.clone().unwrap_or_default(),
        model_count: u32::try_from(report.model_count).unwrap_or(u32::MAX),
    }
}

/// Answer a mutation from the live catalog and notify catalog subscribers.
async fn update_response(
    st: &ServerState,
    provider_id: &str,
    change: &ProviderChange,
) -> pb::ProviderUpdate {
    notify_catalog_updated(st);
    pb::ProviderUpdate {
        provider: super::catalog::provider_info(st, provider_id).await,
        discovery: change.discovery.as_ref().map(discovery_outcome),
    }
}

/// Emit `catalog.updated` to global/session SSE subscribers.
pub(crate) fn notify_catalog_updated(st: &ServerState) {
    let payload = serde_json::json!({
        "id": format!(
            "catalog-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0)
        ),
        "type": "catalog.updated",
        "properties": {}
    });
    let _ = st.catalog_updates.send(payload);
}

pub(crate) async fn upsert_provider(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Json(request): Json<pb::UpsertProviderRequest>,
) -> Result<Json<pb::ProviderUpdate>, V1Error> {
    check_provider_id(&provider_id)?;
    if provider_id == "hya" {
        return Err(V1Error::invalid_argument(
            "provider id `hya` is reserved for the offline provider",
        ));
    }
    let change = st
        .provider_control
        .upsert_provider(ProviderUpsert {
            id: provider_id.clone(),
            kind: request.kind.trim().to_owned(),
            base_url: request.base_url.trim().to_owned(),
            api_key: request
                .api_key
                .map(|key| key.trim().to_owned())
                .filter(|key| !key.is_empty()),
        })
        .await
        .map_err(map_control_error)?;
    Ok(Json(update_response(&st, &provider_id, &change).await))
}

pub(crate) async fn refresh_provider(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    body: Option<Json<pb::RefreshProviderRequest>>,
) -> Result<Json<pb::ProviderUpdate>, V1Error> {
    let _ = body;
    check_provider_id(&provider_id)?;
    let change = st
        .provider_control
        .refresh_provider(provider_id.clone())
        .await
        .map_err(map_control_error)?;
    Ok(Json(update_response(&st, &provider_id, &change).await))
}

pub(crate) async fn set_provider_model(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Json(request): Json<pb::SetProviderModelRequest>,
) -> Result<Json<pb::ProviderUpdate>, V1Error> {
    check_provider_id(&provider_id)?;
    let model_id = check_model_id(&request.model_id)?;
    let display_name = request
        .display_name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    if display_name
        .as_deref()
        .is_some_and(|name| name.len() > 256 || name.chars().any(char::is_control))
    {
        return Err(V1Error::invalid_argument(
            "invalid display name: at most 256 characters without control characters",
        ));
    }
    let context_limit = request.context_limit.filter(|limit| *limit > 0);
    let output_limit = request.output_limit.filter(|limit| *limit > 0);
    if let (Some(context), Some(output)) = (context_limit, output_limit)
        && output > context
    {
        return Err(V1Error::invalid_argument(format!(
            "outputLimit {output} exceeds contextLimit {context}"
        )));
    }
    let change = st
        .provider_control
        .set_model(
            provider_id.clone(),
            model_id,
            ProviderModelOverride {
                display_name,
                context_limit,
                output_limit,
                reasoning: request.reasoning,
            },
        )
        .await
        .map_err(map_control_error)?;
    Ok(Json(update_response(&st, &provider_id, &change).await))
}

pub(crate) async fn remove_provider_model(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ProviderUpdate>, V1Error> {
    let request: pb::RemoveProviderModelRequest =
        super::query_request(&[("provider_id", provider_id.as_str())], &query)?;
    check_provider_id(&request.provider_id)?;
    let model_id = check_model_id(&request.model_id)?;
    let change = st
        .provider_control
        .remove_model(provider_id.clone(), model_id)
        .await
        .map_err(map_control_error)?;
    Ok(Json(update_response(&st, &provider_id, &change).await))
}

/// Max output tokens for a probe: 1, except Responses-protocol routes whose
/// upstream (OpenAI) rejects `max_output_tokens` below 16.
fn probe_output_tokens(kind: Option<ProviderKind>) -> u32 {
    match kind {
        Some(
            ProviderKind::OpenAiResponse | ProviderKind::OpenAiCodex | ProviderKind::GrokBuild,
        ) => 16,
        _ => 1,
    }
}

fn finish_label(finish: FinishReason) -> &'static str {
    match finish {
        FinishReason::Stop => "stop",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::Length => "length",
        FinishReason::Cancelled => "cancelled",
        FinishReason::Error => "error",
    }
}

fn probe_error_code(error: &ProviderError) -> String {
    match error {
        ProviderError::HttpStatus { status, .. } => format!("http_{status}"),
        ProviderError::Transport(_) => "transport".to_owned(),
        ProviderError::UnknownModel(_) => "unknown_model".to_owned(),
        ProviderError::Incompatible(_) => "incompatible".to_owned(),
        ProviderError::Decode(_) | ProviderError::Json(_) => "decode".to_owned(),
        ProviderError::AuthExpired { .. } => "auth_expired".to_owned(),
        ProviderError::Http(_) => "provider_error".to_owned(),
    }
}

fn bounded(value: String) -> String {
    match value.char_indices().nth(PROBE_MESSAGE_LIMIT) {
        Some((end, _)) => value[..end].to_owned(),
        None => value,
    }
}

pub(crate) async fn test_provider_model(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Json(request): Json<pb::TestProviderModelRequest>,
) -> Result<Json<pb::TestProviderModelResponse>, V1Error> {
    check_provider_id(&provider_id)?;
    let model_id = check_model_id(&request.model_id)?;
    let snapshot = st.engine.provider_catalog_snapshot();
    if !snapshot
        .models()
        .iter()
        .any(|row| row.provider_id == provider_id && row.model_id == model_id)
    {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("model not found: {provider_id}/{model_id}"),
        ));
    }
    let kind = snapshot
        .providers()
        .iter()
        .find(|state| state.provider_id == provider_id)
        .map(|state| state.kind);
    let model = ModelRef::new(format!("{provider_id}/{model_id}"));
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        PROBE_TIMEOUT,
        st.engine.probe_model(&model, probe_output_tokens(kind)),
    )
    .await;
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let response = match outcome {
        Ok(Ok(reply)) => pb::TestProviderModelResponse {
            ok: true,
            text: bounded(reply.text),
            finish_reason: reply
                .finish
                .map(finish_label)
                .unwrap_or_default()
                .to_owned(),
            error_code: String::new(),
            error_message: String::new(),
            latency_ms,
        },
        Ok(Err(error)) => pb::TestProviderModelResponse {
            ok: false,
            text: String::new(),
            finish_reason: String::new(),
            error_code: probe_error_code(&error),
            error_message: bounded(error.to_string()),
            latency_ms,
        },
        Err(_) => pb::TestProviderModelResponse {
            ok: false,
            text: String::new(),
            finish_reason: String::new(),
            error_code: "timeout".to_owned(),
            error_message: format!(
                "no complete reply within {} seconds",
                PROBE_TIMEOUT.as_secs()
            ),
            latency_ms,
        },
    };
    Ok(Json(response))
}
