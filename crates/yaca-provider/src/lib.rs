//! `yaca-provider` — Provider/Protocol/Route abstraction normalizing every LLM
//! into the canonical `yaca_proto::Event` stream (design.md §4, the keystone).

pub mod anthropic;
pub mod dev;
pub mod fake;
pub mod google;
pub mod http;
pub mod openai;
pub mod router;
mod wire;

use async_trait::async_trait;
use futures::stream::BoxStream;
use thiserror::Error;
use yaca_proto::{Event, Message, MessageId, ModelRef, SessionId, ToolSchema};

pub use anthropic::{AnthropicDecoder, AnthropicMessagesProtocol};
pub use dev::DevProvider;
pub use fake::{FakeProvider, FakeStep};
pub use google::{GoogleDecoder, GoogleProtocol};
pub use http::{HttpProvider, ProviderKind};
pub use openai::{OpenAiChatDecoder, OpenAiChatProtocol};
pub use router::ProviderRouter;

/// Reject a request a route cannot serve before a turn starts (risk #12):
/// tool-using turns require `streaming_tool_calls`.
pub fn preflight(caps: &Capabilities, req: &CompletionRequest) -> Result<(), ProviderError> {
    if !req.tools.is_empty() && !caps.streaming_tool_calls {
        return Err(ProviderError::Incompatible(
            "route does not support streaming tool calls".to_string(),
        ));
    }
    Ok(())
}

pub type EventStream = BoxStream<'static, Result<Event, ProviderError>>;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http: {0}")]
    Http(String),
    #[error("unknown provider for model: {0}")]
    UnknownModel(String),
    #[error("incompatible route: {0}")]
    Incompatible(String),
    #[error("decode: {0}")]
    Decode(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub streaming_tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub usage_reporting: bool,
    pub json_output: bool,
    pub reasoning_stream: bool,
    pub reasoning_request: bool,
    pub max_context: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    #[must_use]
    pub fn anthropic_budget(self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 4096,
            Self::High => 16384,
        }
    }

    #[must_use]
    pub fn google_budget(self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 8192,
            Self::High => 24576,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub model: ModelRef,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub reasoning: Option<ReasoningEffort>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities>;
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError>;
}

pub trait Protocol: Send + Sync {
    fn encode(&self, req: &CompletionRequest) -> Result<serde_json::Value, ProviderError>;
    fn decoder(&self, session: SessionId, message: MessageId) -> Box<dyn Decoder>;
}

pub trait Decoder: Send {
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError>;
    fn finish(&mut self) -> Result<Vec<Event>, ProviderError>;
}
