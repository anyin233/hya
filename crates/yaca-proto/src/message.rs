//! Message / Part tagged unions (design.md §3). Timestamps live on the DB rows
//! and the `Envelope`, not on these value types (added per-need in later phases).

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, PartId, ToolCallId};
use crate::model::{AgentName, ModelRef, ToolName};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt: u64,
    pub completion: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub input_usd: f64,
    pub output_usd: f64,
}

/// Lifecycle of a tool call as it streams: pending → running → completed | error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ToolPartState {
    Pending {
        input: serde_json::Value,
    },
    Running {
        input: serde_json::Value,
    },
    Completed {
        input: serde_json::Value,
        output: serde_json::Value,
        time_ms: u64,
    },
    Error {
        input: serde_json::Value,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        id: PartId,
        text: String,
    },
    Reasoning {
        id: PartId,
        text: String,
    },
    Tool {
        id: PartId,
        call_id: ToolCallId,
        name: ToolName,
        state: ToolPartState,
    },
}

/// A full message value. Phase 1 covers the core three roles; synthetic /
/// agent-switched / model-switched / compaction variants are added with the
/// phases that emit them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        id: MessageId,
        parts: Vec<Part>,
    },
    Assistant {
        id: MessageId,
        agent: AgentName,
        model: ModelRef,
        parts: Vec<Part>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish: Option<FinishReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<TokenUsage>,
    },
    System {
        id: MessageId,
        content: String,
    },
}
