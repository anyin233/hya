//! `yaca-provider` — Provider/Protocol/Route abstraction normalizing every LLM
//! into the canonical `yaca_proto::Event` stream (design.md §4, the keystone).

pub mod fake;
pub mod openai;
pub mod router;

use async_trait::async_trait;
use futures::stream::BoxStream;
use thiserror::Error;
use yaca_proto::{Event, Message, MessageId, ModelRef, SessionId, ToolSchema};

pub use fake::{FakeProvider, FakeStep};
pub use openai::{OpenAiChatDecoder, OpenAiChatProtocol};
pub use router::ProviderRouter;

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
    pub max_context: u32,
}

#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub model: ModelRef,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
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
