//! Handshake, tool-call, and event wire payloads + protocol constants.
//!
//! These types are the ABI an external plugin author serializes on stdio.
//! Keep them aligned with `docs/plugin-protocol.md`.

use std::collections::BTreeMap;

use hya_proto::{
    Envelope, Message, MessageId, ModelRef, PartId, SessionId, ToolCallId, ToolSchema,
};
use hya_tool::Action;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Re-export of the workspace-adapter metadata shape returned at initialize.
pub use hya_proto::WorkspaceAdapterInfo;

/// Host/plugin protocol major version; initialize must negotiate this value (`1`).
pub const PROTOCOL_VERSION: u32 = 1;

/// JSON-RPC method name for the host→plugin handshake request.
pub const METHOD_INITIALIZE: &str = "initialize";
/// JSON-RPC method name for graceful plugin teardown.
pub const METHOD_SHUTDOWN: &str = "shutdown";
/// Notification method used to fan session envelopes to plugins that registered `event`.
pub const METHOD_EVENT: &str = "event";
/// JSON-RPC method name for invoking a plugin-declared tool.
pub const METHOD_TOOL_CALL: &str = "tool/call";
/// Prefix for hook method names on the wire (`hook/` + [`HookName::as_str`]).
pub const HOOK_METHOD_PREFIX: &str = "hook/";

/// Named hooks a plugin may register during initialize.
///
/// Wire names use dotted snake-style strings (serde `rename`). Only hooks listed
/// in the initialize reply are ever dispatched to that plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HookName {
    /// Fan-out of canonical session [`Envelope`]s (notification, no reply).
    #[serde(rename = "event")]
    Event,
    /// Runs before a slash/command prompt is admitted.
    #[serde(rename = "command.execute.before")]
    CommandExecuteBefore,
    /// Experimental: rewrite completed assistant text before it is finalized.
    #[serde(rename = "experimental.text.complete")]
    TextComplete,
    /// Runs before a user message is admitted (may rewrite text).
    #[serde(rename = "message.user.before")]
    MessageUserBefore,
    /// Adjust completion request parameters (model, temperature, headers, …).
    #[serde(rename = "chat.params")]
    ChatParams,
    /// Guard before a tool runs; may continue with new input or veto.
    #[serde(rename = "tool.execute.before")]
    ToolExecuteBefore,
    /// Observe/adjust tool result after execution.
    #[serde(rename = "tool.execute.after")]
    ToolExecuteAfter,
    /// Intercept a permission ask; may allow, reject, or defer to the user.
    #[serde(rename = "permission.ask")]
    PermissionAsk,
    /// Goal-mode evaluator hook.
    #[serde(rename = "goal.evaluate")]
    GoalEvaluate,
    /// Loop-mode verifier hook.
    #[serde(rename = "loop.verifier")]
    LoopVerifier,
    /// Loop-mode planner hook.
    #[serde(rename = "loop.planner")]
    LoopPlanner,
}

impl HookName {
    /// Wire name without the `hook/` prefix (for example `"tool.execute.before"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HookName::Event => "event",
            HookName::CommandExecuteBefore => "command.execute.before",
            HookName::TextComplete => "experimental.text.complete",
            HookName::MessageUserBefore => "message.user.before",
            HookName::ChatParams => "chat.params",
            HookName::ToolExecuteBefore => "tool.execute.before",
            HookName::ToolExecuteAfter => "tool.execute.after",
            HookName::PermissionAsk => "permission.ask",
            HookName::GoalEvaluate => "goal.evaluate",
            HookName::LoopVerifier => "loop.verifier",
            HookName::LoopPlanner => "loop.planner",
        }
    }

    /// Full JSON-RPC method string (`hook/<wire-name>`).
    #[must_use]
    pub fn method(self) -> String {
        format!("{HOOK_METHOD_PREFIX}{}", self.as_str())
    }

    /// Parse a wire hook name (without `hook/`). Unknown strings return `None`.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "event" => HookName::Event,
            "command.execute.before" => HookName::CommandExecuteBefore,
            "experimental.text.complete" => HookName::TextComplete,
            "message.user.before" => HookName::MessageUserBefore,
            "chat.params" => HookName::ChatParams,
            "tool.execute.before" => HookName::ToolExecuteBefore,
            "tool.execute.after" => HookName::ToolExecuteAfter,
            "permission.ask" => HookName::PermissionAsk,
            "goal.evaluate" => HookName::GoalEvaluate,
            "loop.verifier" => HookName::LoopVerifier,
            "loop.planner" => HookName::LoopPlanner,
            _ => return None,
        })
    }

    /// Default failure policy when the registration omits posture.
    ///
    /// `permission.ask` and `tool.execute.before` default to [`HookPosture::Safe`];
    /// all other hooks default to [`HookPosture::Open`].
    #[must_use]
    pub fn default_posture(self) -> HookPosture {
        match self {
            HookName::PermissionAsk | HookName::ToolExecuteBefore => HookPosture::Safe,
            _ => HookPosture::Open,
        }
    }
}

/// Per-hook failure policy on the wire (`safe` / `open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPosture {
    /// Treat hook failure/timeout as a veto of the guarded action.
    Safe,
    /// Log/skip hook failure; the pipeline continues with prior input.
    Open,
}

/// Host→plugin `initialize` request params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Protocol version the host speaks (must match [`PROTOCOL_VERSION`]).
    pub protocol_version: u32,
    /// Identifying host process metadata.
    pub host: HostInfo,
}

/// Whether a Bundle activation is one-shot or long-lived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationLifecycle {
    /// Activation ends with the turn / binding.
    Transient,
    /// Activation stays resident across turns.
    Resident,
}

/// Extra initialize fields for Bundle activation handshakes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationMetadata {
    /// Host-assigned activation identifier.
    pub activation_id: String,
    /// Transient vs resident lifecycle for this activation.
    pub lifecycle: ActivationLifecycle,
}

/// Host identity sent during initialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Host product name (for example `"hya"`).
    pub name: String,
    /// Host version string.
    pub version: String,
}

/// Plugin→host initialize result: declaration of hooks, tools, and adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// Protocol version the plugin accepts (must equal host [`PROTOCOL_VERSION`]).
    pub protocol_version: u32,
    /// Declared plugin identity; `id` must match the configured source id.
    pub plugin: PluginInfo,
    /// Hooks this plugin implements (only these are dispatched).
    #[serde(default)]
    pub hooks: Vec<HookRegistration>,
    /// Tools this plugin exposes to the model (must use object `inputSchema`).
    #[serde(default)]
    pub tools: Vec<ToolInfo>,
    /// Workspace adapters aggregated for `GET /experimental/workspace/adapter`.
    #[serde(default, rename = "workspaceAdapters")]
    pub workspace_adapters: Vec<WorkspaceAdapterInfo>,
}

/// Identity fields inside [`InitializeResult::plugin`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Stable plugin id (must equal configured / manifest id).
    pub id: String,
    /// Free-form plugin version string.
    pub version: String,
    /// Implementation kind on the wire (`rust`, `compat`, …).
    pub kind: PluginKindWire,
}

/// Plugin implementation kind as serialized on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKindWire {
    /// Native Rust (or other) stdio binary speaking this ABI.
    #[default]
    Rust,
    // Back-compat: existing configs may still declare `kind: opencode` for the
    // external JS-plugin adapter; keep accepting that literal (external contract).
    /// Compat/OpenCode JS plugin via the Bun adapter (`opencode` alias accepted).
    #[serde(alias = "opencode")]
    Compat,
    /// Any other declared kind.
    Other,
}

/// One hook entry in the initialize declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRegistration {
    /// Hook identity.
    pub name: HookName,
    /// Optional posture override; omit to use [`HookName::default_posture`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posture: Option<HookPosture>,
}

/// Tool declaration in the initialize reply (`inputSchema` is camelCase on the wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    /// Tool name exposed to the model.
    pub name: String,
    /// Human-readable description for the model.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for tool input; `type` must be the string `"object"` or the host drops the tool.
    pub input_schema: Value,
}

/// Host→plugin `tool/call` request params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallParams {
    /// Declared tool name.
    pub tool: String,
    /// Session in which the call runs (required by the host adapter).
    pub session: SessionId,
    /// Fresh call id minted by the host for this invocation.
    pub call: ToolCallId,
    /// Tool input object.
    pub input: Value,
}

/// Plugin→host `tool/call` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallReply {
    /// Whether the tool succeeded.
    pub ok: bool,
    /// Structured output (stringified into `ToolError` when `ok` is false).
    #[serde(default)]
    pub output: Value,
    /// Optional wall-clock duration of the tool body in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
}

/// Params for the `event` notification (no reply).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventNotificationParams {
    /// Canonical session envelope being fanned out.
    pub envelope: Envelope,
}

/// Completion request shape embedded in chat-params hooks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCompletionRequest {
    /// Model ref for the upcoming completion.
    pub model: ModelRef,
    /// Optional system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Conversation messages for the provider.
    pub messages: Vec<Message>,
    /// Tool schemas available for this turn.
    pub tools: Vec<ToolSchema>,
    /// Optional sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Optional max output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Optional reasoning-effort label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Extra request headers (plugin may adjust via `chat.params`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// Tool execution result on the wire (after hooks / tool body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WireToolResult {
    /// Successful tool body.
    Ok {
        /// Structured tool output.
        output: Value,
        /// Elapsed milliseconds (default 0 when omitted).
        #[serde(default)]
        time_ms: u64,
    },
    /// Failed tool body.
    Err {
        /// Error message for the model / transcript.
        message: String,
    },
}

/// Permission resource subject as serialized for `permission.ask`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireResource {
    /// Named tool resource.
    Tool {
        /// Tool name or pattern value.
        value: String,
    },
    /// Filesystem path resource.
    Path {
        /// Absolute or relative path.
        value: String,
    },
    /// Glob pattern resource.
    Glob {
        /// Glob string.
        value: String,
    },
    /// Shell command resource.
    Command {
        /// Command string.
        value: String,
    },
    /// Subagent identity resource.
    Subagent {
        /// Subagent name or id.
        value: String,
    },
    /// URL resource.
    Url {
        /// URL string.
        value: String,
    },
    /// Web-search query resource.
    WebSearch {
        /// Query string.
        value: String,
    },
    /// Skill identity resource.
    Skill {
        /// Skill name.
        value: String,
    },
    /// Unrestricted / catch-all resource.
    Any,
}

/// Params for `hook/message.user.before`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageUserBeforeParams {
    /// Session receiving the user message.
    pub session: SessionId,
    /// Current user text (may be rewritten by the outcome).
    pub text: String,
}

/// Params for `hook/command.execute.before`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandExecuteBeforeParams {
    /// Session running the command.
    pub session: SessionId,
    /// Command name without leading `/`.
    pub command: String,
    /// Command argument string.
    pub arguments: String,
    /// Full admitted text (may include synthesized `/cmd args`).
    pub text: String,
}

/// Params for `hook/experimental.text.complete`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextCompleteParams {
    /// Session owning the assistant message.
    pub session: SessionId,
    /// Assistant message id.
    pub message: MessageId,
    /// Text part id being finalized.
    pub part: PartId,
    /// Completed text content.
    pub text: String,
}

/// Params for `hook/chat.params`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatParamsParams {
    /// Session for the upcoming completion.
    pub session: SessionId,
    /// Assistant message id being prepared.
    pub message: MessageId,
    /// Full completion request the host intends to send.
    pub request: WireCompletionRequest,
}

/// Params for `hook/tool.execute.before`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecuteBeforeParams {
    /// Session of the tool call.
    pub session: SessionId,
    /// Assistant message that requested the tool.
    pub message: MessageId,
    /// Tool call id.
    pub call: ToolCallId,
    /// Tool name.
    pub tool: String,
    /// Proposed tool input (may be rewritten on continue).
    pub input: Value,
}

/// Params for `hook/tool.execute.after`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecuteAfterParams {
    /// Session of the tool call.
    pub session: SessionId,
    /// Assistant message that requested the tool.
    pub message: MessageId,
    /// Tool call id.
    pub call: ToolCallId,
    /// Tool name.
    pub tool: String,
    /// Input that was executed.
    pub input: Value,
    /// Tool body result (may be rewritten on continue).
    pub result: WireToolResult,
}

/// Params for `hook/permission.ask`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionAskParams {
    /// Session when known (some asks are process-global).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// Permission action being requested.
    pub action: Action,
    /// Subject of the permission ask.
    pub resource: WireResource,
}

/// Outcome for `message.user.before` (only continue-with-text is defined).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MessageUserBeforeOutcomeWire {
    /// Admit the (possibly rewritten) user text.
    Continue {
        /// Text to admit.
        text: String,
    },
}

/// Outcome for `command.execute.before`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandBeforeOutcomeWire {
    /// Continue with the given command text.
    Continue {
        /// Command prompt text to admit.
        text: String,
    },
}

/// Outcome for `experimental.text.complete`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TextCompleteOutcomeWire {
    /// Accept the (possibly rewritten) completed text.
    Continue {
        /// Final text for the part.
        text: String,
    },
}

/// Outcome for `chat.params`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ChatParamsOutcomeWire {
    /// Proceed with the (possibly rewritten) completion request.
    Continue {
        /// Next completion request.
        request: WireCompletionRequest,
    },
}

/// Outcome for `tool.execute.before`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolBeforeOutcomeWire {
    /// Run the tool with this input.
    Continue {
        /// Input object passed to the tool body.
        input: Value,
    },
    /// Refuse the tool call; host surfaces `reason` to the model.
    Veto {
        /// Human-readable veto reason.
        reason: String,
    },
}

/// Outcome for `tool.execute.after`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolAfterOutcomeWire {
    /// Accept the (possibly rewritten) tool result.
    Continue {
        /// Result recorded for the tool call.
        result: WireToolResult,
    },
}

/// Outcome for `permission.ask`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcomeWire {
    /// Allow this invocation once.
    AllowOnce,
    /// Allow and remember for matching future asks.
    AllowAlways,
    /// Deny the action.
    Reject {
        /// Optional feedback shown to the model inside the denial error.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    /// Let the next plugin or the user-ask path decide.
    Defer,
}
