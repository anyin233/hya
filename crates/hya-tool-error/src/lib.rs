//! Shared tool errors used by the tool plane and the native rewrite contract.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Source input for the lockstep native bundle ABI digest.
#[doc(hidden)]
pub const NATIVE_ABI_SOURCE: &[u8] = include_bytes!("lib.rs");

/// Resource operation category used by legacy rules and user-facing asks.
///
/// Serialized lowercase in saved-permission rows (`read`, `edit`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Invocation-level native tool subject (generic tool call).
    Tool,
    /// Read file/directory contents (`read`, `ls`).
    Read,
    /// Mutate file contents (`write`, `edit`, `apply_patch`).
    Edit,
    /// Path pattern search (`glob`, `find`).
    Glob,
    /// Content regex search (`grep`).
    Grep,
    /// Bash command execution (`bash`).
    Bash,
    /// Subagent spawn (`task`).
    Task,
    /// MCP tool call (`mcp__…`).
    Mcp,
    /// HTTP(S) fetch (`webfetch`).
    WebFetch,
    /// Provider-backed web search (`websearch`).
    WebSearch,
    /// Legacy permission action for historical todo tools.
    TodoWrite,
    /// Load a skill by name (`skill`).
    Skill,
    /// Language-server query (`lsp`).
    Lsp,
    /// Path outside the session workdir trust boundary (never covered by call grant).
    ExternalDirectory,
}

/// Object of a resource-level permission check.
///
/// Flattened via [`Resource::pattern`] for `*` wildcard matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Tool name for invocation-level tool subjects.
    Tool(String),
    /// Resolved filesystem path (display form).
    Path(String),
    /// Glob or grep pattern string.
    Glob(String),
    /// Shell command text, or namespaced MCP tool name.
    Command(String),
    /// Subagent / agent type id.
    Subagent(String),
    /// Fetched URL.
    Url(String),
    /// Web-search query string.
    WebSearch(String),
    /// Skill name.
    Skill(String),
    /// Matches every pattern (`pattern()` → `"*"`); used for whole-action grants.
    Any,
}

impl Resource {
    /// Single string used by the `*` wildcard rule matcher.
    #[must_use]
    pub fn pattern(&self) -> String {
        match self {
            Resource::Tool(s)
            | Resource::Path(s)
            | Resource::Glob(s)
            | Resource::Command(s)
            | Resource::Subagent(s)
            | Resource::Url(s)
            | Resource::WebSearch(s)
            | Resource::Skill(s) => s.clone(),
            Resource::Any => "*".to_string(),
        }
    }
}

/// Denial or infrastructure failure from the permission plane.
#[derive(Error, Debug)]
pub enum PermissionError {
    /// Explicit deny or user reject.
    #[error("permission denied: {action:?} on {resource:?}{}", .feedback.as_deref().map_or(String::new(), |f| format!(" — user says: {f}")))]
    Denied {
        /// Action that was refused.
        action: Action,
        /// Resource that was refused.
        resource: Resource,
        /// Optional human feedback from a reject decision.
        feedback: Option<String>,
    },
    /// Ask channel closed or dropped before a reply arrived.
    #[error("permission channel unavailable")]
    Unavailable,
}

/// Failure returned from tool execution and mapped to wire `error.type` strings by the engine.
#[derive(Error, Debug)]
pub enum ToolError {
    /// Caller-supplied arguments failed validation or schema checks.
    #[error("input: {0}")]
    Input(String),
    /// Permission plane denied the call (or ask channel was unavailable).
    #[error(transparent)]
    Permission(#[from] PermissionError),
    /// Filesystem or process I/O failure during the tool body.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON parse/serialize failure for tool input or output.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Cooperative cancellation via the tool call’s cancellation token.
    #[error("cancelled")]
    Cancelled,
    /// Transient capacity pressure (for example spawn admission overloaded).
    #[error("overloaded: {0}")]
    Overloaded(String),
    /// App-owned Workflow control rejected a structured command.
    #[error("{code}: {message}")]
    WorkflowControl {
        /// Machine-stable control code.
        code: String,
        /// Bounded diagnostic.
        message: String,
    },
    /// Requested subagent type is not in the caller's authorized roster.
    #[error("UNKNOWN_AGENT_ID: `{agent_id}`")]
    UnknownAgentId {
        /// Agent id that was requested.
        agent_id: String,
    },
    /// Caller is not allowed to spawn the named agent (`can_spawn` / roster).
    #[error("AGENT_SPAWN_NOT_ALLOWED: `{caller}` cannot spawn `{agent_id}`")]
    AgentSpawnNotAllowed {
        /// Calling agent id.
        caller: String,
        /// Target agent id that was refused.
        agent_id: String,
    },
    /// Inline agent overlay used a field the runtime does not support.
    #[error("UNSUPPORTED_INLINE_AGENT_FIELD: `{field}`")]
    UnsupportedInlineAgentField {
        /// Unsupported field name.
        field: &'static str,
    },
    /// Catch-all message mapped to wire type `unknown`.
    #[error("{0}")]
    Other(String),
}
