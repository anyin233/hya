//! One ordered hook chain per dispatch site (ADR-0040).
//!
//! Every hook source (a native bundle, a JS bundle through the plugin host,
//! or a core built-in) is an [`hya_runtime_abi::NativeHook`]. A [`HookChain`]
//! holds [`HookEntry`] values in dispatch order; an entry adds the scope (which
//! hook names it admits, whether it sees events) and keeps its source alive.
//! The `hya-runtime-abi` input and outcome types are the only hook payloads.

use hya_provider::ProviderError;

mod activation;
mod chain;

pub(crate) use activation::{
    activation_hook_for, replace_activation_hooks, scope_activation_hooks,
    scope_optional_activation_hooks,
};
pub use chain::{HookChain, HookEntry};
pub use hya_runtime_abi::{
    HookPoint, NativeAgentSpawnInput, NativeChatParamsInput, NativeCommandBeforeInput,
    NativeCompactionAfterInput, NativeCompactionBeforeInput, NativeCompactionDecision,
    NativeCompactionTrigger, NativeHook, NativeMessageUserBeforeInput, NativeModelFallbackInput,
    NativePermissionApproveInput, NativeSessionLifecycleInput, NativeTextCompleteInput,
    NativeToolAfterInput, NativeToolAfterOutcome, NativeToolBeforeInput, NativeToolBeforeOutcome,
    NativeToolResult, ToolVisibilityInput, ToolVisibilityOutcome,
};

/// Classification of a pre-stream provider failure, carried to
/// `model.fallback` hooks as [`NativeModelFallbackInput::error_class`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelFailureClass {
    /// Transient: transport, rate limit, or server error.
    Retryable,
    /// The provider does not know the model.
    UnknownModel,
    /// Credentials expired or were rejected.
    Auth,
    /// The request was rejected as invalid.
    InvalidRequest,
    /// Anything else.
    Other,
}

impl ModelFailureClass {
    /// Classify a provider error.
    #[must_use]
    pub fn of(error: &ProviderError) -> Self {
        if error.is_retryable_before_stream() {
            return Self::Retryable;
        }
        match error {
            ProviderError::UnknownModel(_) => Self::UnknownModel,
            ProviderError::AuthExpired { .. }
            | ProviderError::HttpStatus {
                status: 401 | 403, ..
            } => Self::Auth,
            ProviderError::HttpStatus { status, .. } if (400..500).contains(status) => {
                Self::InvalidRequest
            }
            ProviderError::Incompatible(_) => Self::InvalidRequest,
            _ => Self::Other,
        }
    }

    /// Wire spelling (`retryable`, `unknown_model`, `auth`, `invalid_request`, `other`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::UnknownModel => "unknown_model",
            Self::Auth => "auth",
            Self::InvalidRequest => "invalid_request",
            Self::Other => "other",
        }
    }
}

/// JSON shape of a completion request on `chat.params` (`params`).
#[derive(serde::Serialize, serde::Deserialize)]
struct ChatParamsWire {
    model: hya_proto::ModelRef,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    messages: Vec<hya_proto::Message>,
    #[serde(default)]
    tools: Vec<hya_proto::ToolSchema>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
}

/// Encode a completion request as the `chat.params` payload.
#[must_use]
pub fn chat_params_value(request: &hya_provider::CompletionRequest) -> serde_json::Value {
    serde_json::to_value(ChatParamsWire {
        model: request.model.clone(),
        system: request.system.clone(),
        messages: request.messages.clone(),
        tools: request.tools.clone(),
        temperature: request.temperature,
        max_output_tokens: request.max_output_tokens,
        reasoning: request.reasoning.map(|r| r.as_str().to_string()),
        headers: request.headers.clone(),
    })
    .unwrap_or(serde_json::Value::Null)
}

/// Apply a `chat.params` payload to `request`; a payload that does not
/// decode leaves the request unchanged.
pub fn apply_chat_params(request: &mut hya_provider::CompletionRequest, value: serde_json::Value) {
    let Ok(wire) = serde_json::from_value::<ChatParamsWire>(value) else {
        tracing::warn!("chat.params hooks returned an undecodable request; keeping the original");
        return;
    };
    request.model = wire.model;
    request.system = wire.system;
    request.messages = wire.messages;
    request.tools = wire.tools;
    request.temperature = wire.temperature;
    request.max_output_tokens = wire.max_output_tokens;
    if let Some(reasoning) = wire
        .reasoning
        .as_deref()
        .and_then(hya_provider::ReasoningEffort::parse)
    {
        request.reasoning = Some(reasoning);
    }
    request.headers = wire.headers;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
