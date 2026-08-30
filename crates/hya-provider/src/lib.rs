//! `hya-provider` — Provider/Protocol/Route abstraction normalizing every LLM
//! into the canonical `hya_proto::Event` stream (design.md §4, the keystone).
//!
//! # Adding a backend
//!
//! 1. Implement [`Protocol`] (`encode` + `decoder`) for the upstream JSON/SSE shape.
//! 2. Implement [`Decoder`] as a stateful SSE frame consumer that emits batches of
//!    `hya_proto::Event` (SSE framing itself is owned by [`HttpProvider`], not the decoder).
//! 3. Implement [`Provider`] (`id`, `capabilities`, `stream`) — or wrap the protocol
//!    in [`HttpProvider`] for HTTP+SSE routes.
//! 4. Register the provider on a [`ProviderRouter`] with [`ProviderRouter::with`].
//!
//! Call order for a live turn: router [`ProviderRouter::resolve`] → [`preflight`] →
//! [`Provider::stream`] → HTTP SSE → [`Decoder::push`] / [`Decoder::finish`].

use std::{collections::BTreeMap, time::Duration};

/// Anthropic Messages protocol and stream decoder.
pub mod anthropic;
/// Offline echo provider for local/dev runs without API keys.
pub mod dev;
/// Scripted provider for tests and deterministic agent-loop fixtures.
pub mod fake;
/// Gemini generateContent protocol and decoder.
pub mod google;
/// Generic HTTP+SSE driver shared by OpenAI-compatible, Responses, Anthropic, Google, Grok.
pub mod http;
/// OpenAI Chat Completions and Responses protocols and decoders.
pub mod openai;
/// Ordered model routing with safe pre-stream provider failover.
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
pub use http::{AuthRefresher, BearerResolver, HttpProvider, ProviderKind};
pub use openai::{
    COMPACT_CONTEXT_MARKER, OpenAiChatDecoder, OpenAiChatProtocol, OpenAiResponsesDecoder,
    OpenAiResponsesProtocol, RESPONSES_COMPACT_ITEMS_MARKER, encode_input_items,
    format_responses_compact_system, parse_responses_compact_items,
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

/// Boxed stream of canonical events (or stream-level errors) from [`Provider::stream`].
pub type EventStream = BoxStream<'static, Result<Event, ProviderError>>;

/// Failures from encoding, HTTP, routing, decoding, or auth refresh.
#[derive(Error, Debug)]
pub enum ProviderError {
    /// JSON body or frame (de)serialization failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Provider API error frame, bad header, or HTTP client construction failure.
    #[error("http: {0}")]
    Http(String),
    /// Request transport failed before an HTTP response established the event stream.
    #[error("transport: {0}")]
    Transport(String),
    /// Upstream returned a non-success HTTP response before the event stream began.
    #[error("http status {status}: {message}")]
    HttpStatus {
        /// Numeric HTTP status returned by the upstream provider.
        status: u16,
        /// Bounded response-body detail for diagnostics.
        message: String,
        /// Provider-requested delay parsed from `Retry-After`, when present.
        retry_after: Option<Duration>,
    },
    /// No registered route claimed the model ref via [`Provider::capabilities`].
    #[error("unknown provider for model: {0}")]
    UnknownModel(String),
    /// Preflight or protocol cannot serve this request (tools, media, etc.).
    #[error("incompatible route: {0}")]
    Incompatible(String),
    /// Malformed or truncated upstream stream / compact payload.
    #[error("decode: {0}")]
    Decode(String),
    /// OAuth (or other) credentials expired/revoked; user must re-authenticate.
    #[error("auth expired for provider '{provider}': {hint}")]
    AuthExpired {
        /// Configured provider id whose credentials failed.
        provider: String,
        /// Operator-facing recovery hint (e.g. re-login command).
        hint: String,
    },
}

impl ProviderError {
    /// Whether a request that produced no event stream may be retried or failed over safely.
    #[must_use]
    pub fn is_retryable_before_stream(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::HttpStatus { status, .. } => *status == 429 || (500..=599).contains(status),
            Self::Json(_)
            | Self::Http(_)
            | Self::UnknownModel(_)
            | Self::Incompatible(_)
            | Self::Decode(_)
            | Self::AuthExpired { .. } => false,
        }
    }

    /// Delay requested by the upstream for a retryable HTTP response.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::HttpStatus { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Fixed capability flags and context budget advertised when a route claims a model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Route may stream tool-call assembly mid-turn.
    pub streaming_tool_calls: bool,
    /// Multiple tool calls in one assistant turn are allowed.
    pub parallel_tool_calls: bool,
    /// Stream may emit token usage on finish.
    pub usage_reporting: bool,
    /// Structured JSON output mode (unused by current HTTP defaults).
    pub json_output: bool,
    /// Provider streams separate reasoning parts as first-class events (HTTP default off).
    pub reasoning_stream: bool,
    /// Route accepts a reasoning-effort parameter on the request.
    pub reasoning_request: bool,
    /// Advertised context window in tokens.
    pub max_context: u32,
}

pub(crate) fn append_identity_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Option<()> {
    let length = u64::try_from(bytes.len()).ok()?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Some(())
}

pub(crate) fn append_identity_count(output: &mut Vec<u8>, count: usize) -> Option<()> {
    let count = u64::try_from(count).ok()?;
    output.extend_from_slice(&count.to_be_bytes());
    Some(())
}

pub(crate) fn append_identity_optional_bytes(
    output: &mut Vec<u8>,
    value: Option<&str>,
) -> Option<()> {
    match value {
        Some(value) => {
            output.push(1);
            append_identity_bytes(output, value.as_bytes())?;
        }
        None => output.push(0),
    }
    Some(())
}

pub(crate) fn append_capabilities_identity(
    output: &mut Vec<u8>,
    caps: &Capabilities,
) -> Option<()> {
    output.extend_from_slice(&[
        u8::from(caps.streaming_tool_calls),
        u8::from(caps.parallel_tool_calls),
        u8::from(caps.usage_reporting),
        u8::from(caps.json_output),
        u8::from(caps.reasoning_stream),
        u8::from(caps.reasoning_request),
    ]);
    output.extend_from_slice(&caps.max_context.to_be_bytes());
    Some(())
}

/// One model entry published into the aggregated catalog for UI / API listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderModel {
    /// Configured provider id (route id / auth stem).
    pub provider_id: String,
    /// Upstream model id string.
    pub model_id: String,
    /// Caps advertised for this catalog row.
    pub capabilities: Capabilities,
    /// Effort labels this model supports (empty when reasoning is off).
    pub reasoning_variants: Vec<String>,
}

/// Reasoning / thinking effort requested on a completion (ordered for max-pick defaults).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReasoningEffort {
    /// No reasoning parameter (alias wire `none` / `off`).
    Off,
    /// Lowest non-off effort where supported.
    Minimal,
    /// Low effort.
    Low,
    /// Medium effort (alias wire `med`).
    Medium,
    /// High effort.
    High,
    /// Extra-high effort (OpenAI label collapses with Max).
    XHigh,
    /// Maximum effort where the provider advertises it.
    Max,
}

impl ReasoningEffort {
    /// Parse config/UI effort strings (case-insensitive); unknown values → `None`.
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

    /// Canonical lowercase wire name (`none` for [`Self::Off`]).
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

    /// OpenAI `reasoning_effort` label; `Off` omits; `Max` maps to `xhigh`.
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

    /// Anthropic extended-thinking token budget, if this effort enables thinking.
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

    /// Google `thinkingBudget` when effort is high enough; `Max` scales by model id.
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
mod provider_error_tests {
    use std::time::Duration;

    use super::ProviderError;

    #[test]
    fn only_transient_pre_stream_failures_are_retryable() {
        assert!(ProviderError::Transport("reset".to_string()).is_retryable_before_stream());
        for status in [429, 500, 503, 599] {
            assert!(
                ProviderError::HttpStatus {
                    status,
                    message: String::new(),
                    retry_after: None,
                }
                .is_retryable_before_stream(),
                "status {status} should be retryable"
            );
        }
        for status in [400, 401, 403, 404] {
            assert!(
                !ProviderError::HttpStatus {
                    status,
                    message: String::new(),
                    retry_after: Some(Duration::from_secs(1)),
                }
                .is_retryable_before_stream(),
                "status {status} should not be retryable"
            );
        }
        assert!(!ProviderError::Decode("truncated".to_string()).is_retryable_before_stream());
    }
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

/// Normalized completion request shared by every protocol encoder and provider.
#[derive(Clone, Debug)]
pub struct CompletionRequest {
    /// Model ref (bare id, `provider/model`, optional `#variant`).
    pub model: ModelRef,
    /// Optional top-level system prompt (also may appear as system messages).
    pub system: Option<String>,
    /// Conversation messages in canonical `hya_proto` form.
    pub messages: Vec<Message>,
    /// Tool schemas offered for this turn.
    pub tools: Vec<ToolSchema>,
    /// Optional sampling temperature.
    pub temperature: Option<f32>,
    /// Optional max generation tokens.
    pub max_output_tokens: Option<u32>,
    /// Optional reasoning effort; stripped by the router when the route rejects it.
    pub reasoning: Option<ReasoningEffort>,
    /// Extra HTTP headers merged over route auth (sensitive values).
    pub headers: BTreeMap<String, String>,
}

/// Result of a standalone Responses `/responses/compact` call.
#[derive(Clone, Debug)]
pub struct CompactedWindow {
    /// Canonical next input items (pass as-is into the following `/responses` call).
    pub items: Vec<serde_json::Value>,
}

/// A configured model route: claims models, streams completions, optional compaction.
///
/// Minimum implement surface: [`Provider::id`], [`Provider::capabilities`], and
/// [`Provider::stream`]. Returning `Some` from `capabilities` **claims** the model
/// for first-match routing.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Configured provider id (auth filename stem; `providerID` half of model refs).
    fn id(&self) -> &str;
    /// `Some(caps)` claims `model` for this route; `None` leaves it for later routes.
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities>;

    /// Return deterministic configured routing identity, excluding secrets and
    /// transient live state. Providers without a complete identity fail closed.
    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        None
    }

    /// Models this route publishes into the aggregated catalog (default empty).
    fn catalog(&self) -> Vec<ProviderModel> {
        Vec::new()
    }
    /// Stream one completion as canonical events for `session` / `message` ids.
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError>;

    /// When supported (OpenAI Responses / Codex / Grok Build), compact `messages`
    /// via `POST /responses/compact`. Returns `Ok(None)` when the route has no
    /// compact endpoint so callers can fall back to a local summarizer.
    async fn compact_responses(
        &self,
        _model: &ModelRef,
        _messages: &[Message],
        _system: Option<&str>,
    ) -> Result<Option<CompactedWindow>, ProviderError> {
        Ok(None)
    }
}

/// Encoder/decoder pair for one upstream API shape (HTTP body + stream grammar).
///
/// [`Protocol::encode`] builds the request JSON. [`Protocol::decoder`] returns a
/// fresh stateful decoder; the transport (e.g. [`HttpProvider`]) owns SSE framing
/// and feeds frame data into [`Decoder::push`].
pub trait Protocol: Send + Sync {
    /// Encode a normalized request into the upstream JSON body.
    fn encode(&self, req: &CompletionRequest) -> Result<serde_json::Value, ProviderError>;
    /// Construct a decoder bound to this turn's ids and requested effort.
    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn Decoder>;
}

/// Incremental converter from upstream stream fragments into canonical `Event`s.
///
/// Callers pass SSE **data** payloads (not full wire lines) into [`Decoder::push`].
/// [`Decoder::finish`] flushes open parts when the stream ends. Either method may
/// return an empty batch.
pub trait Decoder: Send {
    /// Consume one data fragment; return zero or more events.
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError>;
    /// End of stream: close open parts and emit finish/usage as required.
    fn finish(&mut self) -> Result<Vec<Event>, ProviderError>;
}
