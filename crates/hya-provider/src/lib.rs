//! `hya-provider` — Provider/Protocol/Route abstraction normalizing every LLM
//! into the canonical `hya_proto::Event` stream (design.md §4, the keystone).

use std::collections::BTreeMap;

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
use hya_proto::{Event, Message, MessageId, ModelRef, SessionId, ToolSchema};
use thiserror::Error;

pub use anthropic::{AnthropicDecoder, AnthropicMessagesProtocol};
pub use dev::DevProvider;
pub use fake::{FakeProvider, FakeStep};
pub use google::{GoogleDecoder, GoogleProtocol};
pub use http::{HttpProvider, ProviderKind};
pub use openai::{
    OpenAiChatDecoder, OpenAiChatProtocol, OpenAiResponsesDecoder, OpenAiResponsesProtocol,
};
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderModel {
    pub provider_id: String,
    pub model_id: String,
    pub capabilities: Capabilities,
    pub reasoning_variants: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReasoningEffort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    #[must_use]
    pub fn openai_label(self, _model_id: &str) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh | Self::Max => Some("xhigh"),
        }
    }

    #[must_use]
    pub fn anthropic_budget(self) -> Option<u32> {
        match self {
            Self::Off | Self::Minimal => None,
            Self::Low => Some(1024),
            Self::Medium => Some(4096),
            Self::High => Some(16000),
            Self::XHigh => Some(24000),
            Self::Max => Some(31999),
        }
    }

    #[must_use]
    pub fn google_budget(self, model_id: &str) -> Option<u32> {
        match self {
            Self::Off | Self::Minimal | Self::Low | Self::Medium => None,
            Self::High => Some(16000),
            Self::XHigh => Some(20000),
            Self::Max => {
                let id = model_id.to_ascii_lowercase();
                if id.contains("2.5") && id.contains("pro") {
                    Some(32768)
                } else {
                    Some(24576)
                }
            }
        }
    }
}

/// Precedence: explicit config, then last-used (kept if `Off` or supported),
/// then highest supported. `None` means the model has no reasoning support and
/// must not show a default.
#[must_use]
pub fn resolve_default_reasoning(
    explicit: Option<ReasoningEffort>,
    last_used: Option<ReasoningEffort>,
    supported: &[String],
) -> Option<ReasoningEffort> {
    if explicit.is_some() {
        return explicit;
    }

    let supported_efforts = supported
        .iter()
        .filter_map(|level| ReasoningEffort::parse(level))
        .collect::<Vec<_>>();

    if let Some(effort) = last_used
        && (effort == ReasoningEffort::Off || supported_efforts.contains(&effort))
    {
        return Some(effort);
    }

    supported_efforts.into_iter().max()
}

#[cfg(test)]
mod reasoning_effort_tests {
    use super::ReasoningEffort as R;

    #[test]
    fn parses_compat_vocab() {
        assert_eq!(R::parse("none"), Some(R::Off));
        assert_eq!(R::parse("off"), Some(R::Off));
        assert_eq!(R::parse("minimal"), Some(R::Minimal));
        assert_eq!(R::parse("med"), Some(R::Medium));
        assert_eq!(R::parse("xhigh"), Some(R::XHigh));
        assert_eq!(R::parse("MAX"), Some(R::Max));
        assert_eq!(R::parse("bogus"), None);
    }

    #[test]
    fn openai_never_emits_max() {
        assert_eq!(R::Max.openai_label("gpt-5.5"), Some("xhigh"));
        assert_eq!(R::XHigh.openai_label("gpt-5.5"), Some("xhigh"));
        assert_eq!(R::High.openai_label("gpt-5.5"), Some("high"));
        assert_eq!(R::Off.openai_label("gpt-5.5"), None);
    }

    #[test]
    fn anthropic_budgets_match_compat() {
        assert_eq!(R::High.anthropic_budget(), Some(16000));
        assert_eq!(R::Max.anthropic_budget(), Some(31999));
        assert_eq!(R::Minimal.anthropic_budget(), None);
        assert_eq!(R::Off.anthropic_budget(), None);
    }

    #[test]
    fn google_budgets_by_model() {
        assert_eq!(R::Max.google_budget("gemini-2.5-pro"), Some(32768));
        assert_eq!(R::Max.google_budget("gemini-2.5-flash"), Some(24576));
        assert_eq!(R::High.google_budget("gemini-2.5-flash"), Some(16000));
        assert_eq!(R::Low.google_budget("gemini-2.5-flash"), None);
    }

    #[test]
    fn default_reasoning_keeps_explicit_off() {
        let supported = vec!["low".to_string(), "high".to_string()];

        let resolved = super::resolve_default_reasoning(Some(R::Off), Some(R::High), &supported);

        assert_eq!(resolved, Some(R::Off));
    }

    #[test]
    fn default_reasoning_uses_supported_last_used_before_highest() {
        let supported = vec![
            "minimal".to_string(),
            "low".to_string(),
            "xhigh".to_string(),
        ];

        let resolved = super::resolve_default_reasoning(None, Some(R::Low), &supported);

        assert_eq!(resolved, Some(R::Low));
    }

    #[test]
    fn default_reasoning_ignores_unsupported_last_used_and_picks_highest() {
        let supported = vec!["low".to_string(), "medium".to_string(), "high".to_string()];

        let resolved = super::resolve_default_reasoning(None, Some(R::XHigh), &supported);

        assert_eq!(resolved, Some(R::High));
    }

    #[test]
    fn default_reasoning_picks_max_for_google_or_anthropic_variants() {
        let supported = vec!["high".to_string(), "max".to_string()];

        let resolved = super::resolve_default_reasoning(None, None, &supported);

        assert_eq!(resolved, Some(R::Max));
    }

    #[test]
    fn default_reasoning_stays_unset_without_reasoning_support() {
        let supported = Vec::new();

        let resolved = super::resolve_default_reasoning(None, None, &supported);

        assert_eq!(resolved, None);
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
    pub headers: BTreeMap<String, String>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities>;
    fn catalog(&self) -> Vec<ProviderModel> {
        Vec::new()
    }
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
