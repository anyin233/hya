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

/// Lifecycle status of a spawned subagent member, as observed by the lead/tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRunStatus {
    Spawning,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, alias = "prompt")]
    pub input: u64,
    #[serde(default, alias = "completion")]
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
}

impl TokenUsage {
    #[must_use]
    pub fn is_zero(self) -> bool {
        self == Self::default()
    }

    pub fn merge(&mut self, other: Self) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.reasoning = self.reasoning.max(other.reasoning);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
    }
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
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
    Media {
        id: PartId,
        media_type: String,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
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
