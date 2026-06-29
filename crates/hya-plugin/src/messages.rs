//! Handshake, tool-call, and event wire payloads + protocol constants.

use std::collections::BTreeMap;

use hya_proto::{
    Envelope, Message, MessageId, ModelRef, PartId, SessionId, ToolCallId, ToolSchema,
};
use hya_tool::Action;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use hya_proto::WorkspaceAdapterInfo;

pub const PROTOCOL_VERSION: u32 = 1;

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_SHUTDOWN: &str = "shutdown";
pub const METHOD_EVENT: &str = "event";
pub const METHOD_TOOL_CALL: &str = "tool/call";
pub const HOOK_METHOD_PREFIX: &str = "hook/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HookName {
    #[serde(rename = "event")]
    Event,
    #[serde(rename = "command.execute.before")]
    CommandExecuteBefore,
    #[serde(rename = "experimental.text.complete")]
    TextComplete,
    #[serde(rename = "message.user.before")]
    MessageUserBefore,
    #[serde(rename = "chat.params")]
    ChatParams,
    #[serde(rename = "tool.execute.before")]
    ToolExecuteBefore,
    #[serde(rename = "tool.execute.after")]
    ToolExecuteAfter,
    #[serde(rename = "permission.ask")]
    PermissionAsk,
    #[serde(rename = "goal.evaluate")]
    GoalEvaluate,
    #[serde(rename = "loop.verifier")]
    LoopVerifier,
    #[serde(rename = "loop.planner")]
    LoopPlanner,
}

impl HookName {
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

    #[must_use]
    pub fn method(self) -> String {
        format!("{HOOK_METHOD_PREFIX}{}", self.as_str())
    }

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

    #[must_use]
    pub fn default_posture(self) -> HookPosture {
        match self {
            HookName::PermissionAsk | HookName::ToolExecuteBefore => HookPosture::Safe,
            _ => HookPosture::Open,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPosture {
    Safe,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub host: HostInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol_version: u32,
    pub plugin: PluginInfo,
    #[serde(default)]
    pub hooks: Vec<HookRegistration>,
    #[serde(default)]
    pub tools: Vec<ToolInfo>,
    #[serde(default, rename = "workspaceAdapters")]
    pub workspace_adapters: Vec<WorkspaceAdapterInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
    pub kind: PluginKindWire,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKindWire {
    #[default]
    Rust,
    Opencode,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRegistration {
    pub name: HookName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posture: Option<HookPosture>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallParams {
    pub tool: String,
    pub session: SessionId,
    pub call: ToolCallId,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallReply {
    pub ok: bool,
    #[serde(default)]
    pub output: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventNotificationParams {
    pub envelope: Envelope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCompletionRequest {
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WireToolResult {
    Ok {
        output: Value,
        #[serde(default)]
        time_ms: u64,
    },
    Err {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireResource {
    Path { value: String },
    Glob { value: String },
    Command { value: String },
    Subagent { value: String },
    Url { value: String },
    WebSearch { value: String },
    Skill { value: String },
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageUserBeforeParams {
    pub session: SessionId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandExecuteBeforeParams {
    pub session: SessionId,
    pub command: String,
    pub arguments: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextCompleteParams {
    pub session: SessionId,
    pub message: MessageId,
    pub part: PartId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatParamsParams {
    pub session: SessionId,
    pub message: MessageId,
    pub request: WireCompletionRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecuteBeforeParams {
    pub session: SessionId,
    pub message: MessageId,
    pub call: ToolCallId,
    pub tool: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecuteAfterParams {
    pub session: SessionId,
    pub message: MessageId,
    pub call: ToolCallId,
    pub tool: String,
    pub input: Value,
    pub result: WireToolResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionAskParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    pub action: Action,
    pub resource: WireResource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MessageUserBeforeOutcomeWire {
    Continue { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandBeforeOutcomeWire {
    Continue { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TextCompleteOutcomeWire {
    Continue { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ChatParamsOutcomeWire {
    Continue { request: WireCompletionRequest },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolBeforeOutcomeWire {
    Continue { input: Value },
    Veto { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolAfterOutcomeWire {
    Continue { result: WireToolResult },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcomeWire {
    AllowOnce,
    AllowAlways,
    Reject {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    Defer,
}
