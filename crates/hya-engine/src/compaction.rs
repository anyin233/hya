//! The host's side of context compaction. The reduction itself is the
//! kernel's, with the `hya/compaction` internal bundle as its extension; the
//! host keeps the settings ([`CompactionConfig`]) and the summarizer
//! ([`Summarizer`]) that writes the terminal handoff (ADR-0015).

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use hya_proto::{Event, Message, MessageId, ModelRef, Part, PartId, SessionId, TokenUsage};
use hya_provider::{
    CompletionRequest, EventStream, ProviderError, ProviderRouter, ReasoningEffort,
};

use crate::error::CoreError;

/// Optional overrides for a fixed Harness system summarize/compaction call.
///
/// Absent fields preserve the summarizer's constructed fallback model and leave
/// system/reasoning unset rather than inventing hardcoded prompts.
#[derive(Clone, Debug, Default)]
pub struct SummarizeOptions {
    /// Optional system prompt override for the summarizer call.
    pub system: Option<String>,
    /// Optional model override; defaults to the summarizer's constructed model.
    pub model: Option<ModelRef>,
    /// Optional reasoning effort for capable models.
    pub reasoning: Option<ReasoningEffort>,
    /// The summary this one supersedes, when the session has compacted before.
    ///
    /// Passing it makes compaction incremental: the model updates an existing
    /// anchored summary instead of re-deriving one from prose that is itself
    /// already a summary, which is where detail bleeds away across repeated
    /// compactions.
    pub previous_summary: Option<String>,
    /// Output cap for the summarizer call; `None` keeps the built-in default.
    ///
    /// A structured multi-section summary does not fit the 1024 tokens this
    /// used to hard-code, and a truncated summary loses its trailing sections —
    /// which are the ones describing what to do next.
    pub max_output_tokens: Option<u32>,
    /// Sink for the usage of the provider calls this summarize makes.
    ///
    /// The summarizer records `(serving model, usage)` per call; the engine
    /// drains it into `UsageRecorded { purpose: compaction }` for the session
    /// it serves. `None` drops usage (callers that bill nothing).
    pub usage: Option<UsageCollector>,
}

/// Shared sink collecting `(serving model, usage)` of side provider calls.
///
/// Cloning shares the sink. Recording never fails: a poisoned lock is
/// recovered, since usage accounting must not fail the call it describes.
#[derive(Clone, Debug, Default)]
pub struct UsageCollector(Arc<std::sync::Mutex<Vec<(ModelRef, TokenUsage)>>>);

impl UsageCollector {
    /// Record one provider call's usage.
    pub fn record(&self, model: ModelRef, tokens: TokenUsage) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((model, tokens));
    }

    /// Drain every recorded call, oldest first.
    #[must_use]
    pub fn take(&self) -> Vec<(ModelRef, TokenUsage)> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// Drain a side-call stream into its text and the usage it reported.
///
/// Usage is returned even when the stream fails after reporting it, so a
/// billed call is never dropped from accounting.
pub(crate) async fn collect_text_and_usage(
    mut stream: EventStream,
) -> (Result<String, ProviderError>, Option<TokenUsage>) {
    let mut text = String::new();
    let mut usage: Option<TokenUsage> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(Event::TextDelta { delta, .. }) => text.push_str(&delta),
            Ok(Event::MessageFinished {
                tokens: Some(tokens),
                ..
            }) => usage.get_or_insert_with(TokenUsage::default).merge(tokens),
            Ok(_) => {}
            Err(error) => return (Err(error), usage),
        }
    }
    (Ok(text), usage)
}

/// Compaction settings. `keep_recent` goes to the `hya/compaction` bundle as
/// JSON; `summary_max_tokens` caps the terminal handoff.
#[derive(Clone, Copy, Debug)]
pub struct CompactionConfig {
    /// Number of recent messages whose tool outputs are kept.
    pub keep_recent: usize,
    /// Output cap for the summarizer call that writes the terminal handoff.
    pub summary_max_tokens: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            keep_recent: 6,
            summary_max_tokens: 4_096,
        }
    }
}

/// Writes the terminal handoff document over an ended episode (ADR-0015).
///
/// **Contract:** Must not write the session store. On error the caller falls
/// back to a deterministic degraded handoff.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Write the handoff document for `messages`.
    ///
    /// # Errors
    /// Propagate provider failures as [`CoreError`].
    async fn summarize(
        &self,
        messages: &[Message],
        options: SummarizeOptions,
    ) -> Result<String, CoreError>;
}

/// Section structure of a **terminal** handoff document (ADR-0015).
///
/// The terminal handoff is the only state an archived agent carries into its
/// next episode, so it records what *is* — never how the session got there.
/// Resolved errors fold into Decisions/Current state; there is no
/// errors-and-fixes narrative section.
const STATE_HANDOFF_TEMPLATE: &str = "\
Write a state handoff for an agent resuming this work in a fresh episode, \
under exactly these headings, keeping every heading even when its section is \
empty. Record only current state — never narrate how it came to be:

1. Goal - the task, in the parent's terms.
2. Current state - what exists and works right now.
3. Files and code - exact paths touched, and what changed or matters in each.
4. Decisions - choices in force, including what was ruled out.
5. Pending tasks - work explicitly requested and not yet done.
6. Next step - the single next action, or `none`.

Preserve exact file paths, identifiers, signatures, and command lines. Prefer \
terse bullets over paragraphs.";

/// Request messages for a terminal handoff call: the transcript verbatim,
/// plus exactly one trailing prompt.
///
/// Verbatim matters twice over: the document must cover where the session
/// *stands* — including recent turns — and a cache-capable provider can reuse
/// the live prefix because the request shape matches the conversation it
/// already saw.
#[must_use]
pub fn handoff_request_messages(messages: &[Message], options: &SummarizeOptions) -> Vec<Message> {
    let mut prompt = String::new();
    if let Some(previous) = options
        .previous_summary
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        let _ = write!(
            prompt,
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        );
    }
    let _ = write!(prompt, "{STATE_HANDOFF_TEMPLATE}");
    let mut out = messages.to_vec();
    out.push(Message::User {
        id: MessageId::new(),
        parts: vec![Part::Text {
            id: PartId::new(),
            text: prompt,
        }],
    });
    out
}

/// Output cap used when the caller names none; matches
/// `CompactionConfig::summary_max_tokens`.
const DEFAULT_SUMMARY_MAX_TOKENS: u32 = 4_096;

/// [`Summarizer`] that calls a provider model with no tools.
pub struct ModelSummarizer {
    providers: Arc<ProviderRouter>,
    model: ModelRef,
}

impl ModelSummarizer {
    /// Route summaries through `model` via `providers`.
    #[must_use]
    pub fn new(providers: Arc<ProviderRouter>, model: ModelRef) -> Self {
        Self { providers, model }
    }
}

#[async_trait]
impl Summarizer for ModelSummarizer {
    async fn summarize(
        &self,
        messages: &[Message],
        options: SummarizeOptions,
    ) -> Result<String, CoreError> {
        // The transcript itself plus one trailing prompt, so the model reads
        // the session verbatim (and a cache-capable provider reuses the
        // prefix).
        let request_messages = handoff_request_messages(messages, &options);
        let model = options.model.unwrap_or_else(|| self.model.clone());
        let request = CompletionRequest {
            model: model.clone(),
            system: options.system,
            messages: request_messages,
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: Some(
                options
                    .max_output_tokens
                    .unwrap_or(DEFAULT_SUMMARY_MAX_TOKENS),
            ),
            reasoning: options.reasoning,
            headers: Default::default(),
        };
        let stream = self
            .providers
            .stream(request, SessionId::new(), MessageId::new())
            .await?;
        let (text, usage) = collect_text_and_usage(stream).await;
        if let (Some(collector), Some(tokens)) = (&options.usage, usage)
            && !tokens.is_zero()
        {
            collector.record(model, tokens);
        }
        Ok(text?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: text.to_string(),
            }],
        }
    }

    /// The handoff request carries the transcript verbatim and appends exactly
    /// one prompt message, anchored on any previous summary.
    #[test]
    fn handoff_request_appends_one_prompt_after_the_verbatim_transcript() {
        let msgs = vec![user("hello"), user("world")];
        let options = SummarizeOptions {
            previous_summary: Some("PRIOR".to_string()),
            ..SummarizeOptions::default()
        };
        let out = handoff_request_messages(&msgs, &options);
        assert_eq!(out.len(), msgs.len() + 1);
        assert_eq!(out[0].id(), msgs[0].id(), "transcript is verbatim");
        assert_eq!(out[1].id(), msgs[1].id());
        let Message::User { parts, .. } = out.last().expect("trailing prompt") else {
            panic!("handoff prompt is a user message");
        };
        let Part::Text { text, .. } = &parts[0] else {
            panic!("handoff prompt is text");
        };
        assert!(text.contains("<previous-summary>\nPRIOR\n</previous-summary>"));
        assert!(text.contains("state handoff"), "{text}");
    }
}
