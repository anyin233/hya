//! Dependency-inverted control port for durable per-Agent model preferences.
//!
//! The server owns the wire contract and route-facing error codes. The
//! application runtime supplies the implementation, which keeps persistence,
//! runtime binding, and provider catalog policy out of the HTTP layer.

use futures::future::BoxFuture;
use hya_core::TurnBinding;
use hya_proto::ModelRef;
use serde::{Deserialize, Serialize};

/// Stable code returned when the application did not install the control.
pub const AGENT_MODEL_CONTROL_UNAVAILABLE: &str = "AGENT_MODEL_CONTROL_UNAVAILABLE";
/// Stable code returned when a requested Agent is not in the bound catalog.
pub const AGENT_MODEL_UNKNOWN_AGENT: &str = "AGENT_MODEL_UNKNOWN_AGENT";
/// Stable code returned when an Agent has explicit model configuration.
pub const AGENT_MODEL_CONFIGURED: &str = "AGENT_MODEL_CONFIGURED";
/// Stable code returned when a model is absent from the current catalog.
pub const AGENT_MODEL_UNAVAILABLE: &str = "AGENT_MODEL_UNAVAILABLE";
/// Stable code returned for durable store or runtime control failures.
pub const AGENT_MODEL_CONTROL_FAILURE: &str = "AGENT_MODEL_CONTROL_FAILURE";
/// Stable code returned for malformed route input.
pub const AGENT_MODEL_INVALID_REQUEST: &str = "AGENT_MODEL_INVALID_REQUEST";

/// Stable provider/model identity used by Agent model preference routes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentModelIdentity {
    /// Provider route id that owns the model.
    #[serde(rename = "providerID")]
    pub provider_id: String,
    /// Provider-local model id, including any model-local slashes.
    #[serde(rename = "modelID")]
    pub model_id: String,
}

impl AgentModelIdentity {
    /// Construct a provider/model identity without changing either component.
    ///
    /// # Arguments
    ///
    /// * `provider` - Exact provider route id.
    /// * `model` - Exact provider-local model id.
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_id: provider.into(),
            model_id: model.into(),
        }
    }
}

/// Source used to resolve the effective model shown for one Agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentModelSource {
    /// The Agent has an explicit direct model or category policy.
    Configured,
    /// A durable preference was retained and exactly matches the current catalog.
    Remembered,
    /// No configured or retained model is active; the process base is used.
    Default,
}

/// Effective model and the policy tier that supplied it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentModelEffective {
    /// Effective provider/model identity. Flattening keeps the wire object at
    /// `{providerID, modelID, source}` rather than introducing a nested model.
    #[serde(flatten)]
    pub model: AgentModelIdentity,
    /// Effective source tier.
    pub source: AgentModelSource,
}

/// Normalized durable and effective state for one catalog Agent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelState {
    /// Stable catalog Agent id.
    #[serde(rename = "agentID")]
    pub agent_id: String,
    /// Optional human-readable Agent description.
    pub description: Option<String>,
    /// Selector role (`primary` or `subagent`).
    pub mode: String,
    /// Whether the Agent is hidden from ordinary Agent selection.
    pub hidden: bool,
    /// Whether direct model/category configuration is present.
    pub configured: bool,
    /// Whether a preference can be set for this Agent.
    pub settable: bool,
    /// Retained preference, including stale or configured rows.
    pub preference: Option<AgentModelIdentity>,
    /// Whether the retained preference exactly matches the current model catalog.
    pub preference_available: bool,
    /// Current effective model and its source.
    pub effective: AgentModelEffective,
}

/// Bounded structured failure returned by the Agent model control port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentModelControlError {
    /// Machine-readable stable error code.
    pub code: String,
    /// Bounded human-readable diagnostic.
    pub message: String,
}

impl AgentModelControlError {
    /// Construct one bounded structured control failure.
    ///
    /// Codes are capped at 128 Unicode scalar values and messages at 2,048
    /// Unicode scalar values so provider/store diagnostics cannot grow an API
    /// response without bound.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: bounded(code.into(), 128),
            message: bounded(message.into(), 2_048),
        }
    }

    /// Construct the canonical unavailable-control failure.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(
            AGENT_MODEL_CONTROL_UNAVAILABLE,
            "Agent model control is unavailable",
        )
    }
}

impl std::fmt::Display for AgentModelControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentModelControlError {}

/// Boxed asynchronous result returned by an [`AgentModelControl`] operation.
pub type AgentModelControlFuture<'a, T> = BoxFuture<'a, Result<T, AgentModelControlError>>;

/// Server-owned control port for per-Agent model preference state.
///
/// Implementations are cheap to clone behind `Arc` and safe for concurrent
/// TUI clients. `list` and `set` receive one immutable binding plus the process
/// base model, so each request uses one catalog/preference snapshot.
pub trait AgentModelControl: Send + Sync {
    /// Whether a real application-owned control is installed.
    fn available(&self) -> bool;

    /// List normalized state for every Agent in the supplied binding.
    fn list(
        &self,
        binding: TurnBinding,
        base_model: ModelRef,
    ) -> AgentModelControlFuture<'_, Vec<AgentModelState>>;

    /// Set or clear one Agent preference and return its post-commit state.
    fn set(
        &self,
        binding: TurnBinding,
        agent_id: String,
        preference: Option<AgentModelIdentity>,
        base_model: ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState>;
}

/// Default control used by callers that do not install an application runtime.
pub(crate) struct EmptyAgentModelControl;

impl AgentModelControl for EmptyAgentModelControl {
    fn available(&self) -> bool {
        false
    }

    fn list(
        &self,
        _binding: TurnBinding,
        _base_model: ModelRef,
    ) -> AgentModelControlFuture<'_, Vec<AgentModelState>> {
        Box::pin(async { Err(AgentModelControlError::unavailable()) })
    }

    fn set(
        &self,
        _binding: TurnBinding,
        _agent_id: String,
        _preference: Option<AgentModelIdentity>,
        _base_model: ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        Box::pin(async { Err(AgentModelControlError::unavailable()) })
    }
}

/// Truncate a string by Unicode scalar count only when it exceeds the bound.
fn bounded(value: String, limit: usize) -> String {
    let Some((end, _)) = value.char_indices().nth(limit) else {
        return value;
    };
    value[..end].to_string()
}
