//! Terminal handoff generation (ADR-0015).
//!
//! The terminal handoff is the only state an archived agent carries into its
//! next episode. It is written once, at report time, by the subagent's
//! configured summarizer over the ended episode transcript with the state-only
//! template. Each generation
//! anchors on its predecessor. Generation never fails: a missing summarizer, a
//! failed call, or engine-synthesized terminality falls back to a deterministic
//! projection-derived document flagged `degraded`.

mod read_view;
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use hya_proto::{
    Event, Message, MessageId, ModelRef, Part, PartId, Projection, SessionId, TokenUsage,
    UsagePurpose,
};
use hya_provider::{
    CompletionRequest, EventStream, ProviderError, ProviderRouter, ReasoningEffort,
};

use super::SessionEngine;
use super::models::ProviderSnapshots;
use super::summary::{episode_messages, summary_messages};
use crate::error::CoreError;

/// Optional overrides for one summarizer request.
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
    /// The summary this one supersedes, when a prior handoff exists.
    ///
    /// Passing it makes the handoff incremental: the model updates an existing
    /// anchored summary instead of re-deriving one from prose that is itself
    /// already a summary, which is where detail bleeds away across episodes.
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

/// Settings for generation of terminal handoff documents.
#[derive(Clone, Copy, Debug)]
pub struct TerminalHandoffOptions {
    /// Maximum output tokens for each model-backed terminal handoff.
    pub max_output_tokens: u32,
}

impl Default for TerminalHandoffOptions {
    fn default() -> Self {
        Self {
            max_output_tokens: DEFAULT_SUMMARY_MAX_TOKENS,
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
fn request_messages(messages: &[Message], options: &SummarizeOptions) -> Vec<Message> {
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

/// Output cap used when the caller names none.
const DEFAULT_SUMMARY_MAX_TOKENS: u32 = 4_096;

/// [`Summarizer`] that calls a provider model with no tools.
pub struct ModelSummarizer {
    providers: SummarizerProviders,
    model: ModelRef,
}

enum SummarizerProviders {
    Fixed(Arc<ProviderRouter>),
    Live(ProviderSnapshots),
}

impl ModelSummarizer {
    /// Route summaries through `model` using a fixed router.
    #[must_use]
    pub fn new(providers: Arc<ProviderRouter>, model: ModelRef) -> Self {
        Self {
            providers: SummarizerProviders::Fixed(providers),
            model,
        }
    }

    /// Use the engine's current provider generation for each handoff request.
    #[must_use]
    pub fn for_engine(engine: &SessionEngine, model: ModelRef) -> Self {
        Self {
            providers: SummarizerProviders::Live(engine.providers.clone()),
            model,
        }
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
        let request_messages = request_messages(messages, &options);
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
        let snapshot;
        let router = match &self.providers {
            SummarizerProviders::Fixed(router) => router.as_ref(),
            SummarizerProviders::Live(providers) => {
                snapshot = providers.snapshot();
                snapshot.router()
            }
        };
        let stream = router
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

/// A generated terminal handoff.
#[derive(Clone, Debug)]
pub struct TerminalHandoff {
    /// Episode generation; anchors on the previous document.
    pub generation: u32,
    /// Full six-section state document.
    pub doc: String,
    /// A deterministic projection-derived fallback produced this document.
    pub degraded: bool,
}

impl SessionEngine {
    /// Generate the terminal handoff for `session`. See the module docs for
    /// the degradation contract.
    pub async fn terminal_handoff(
        &self,
        session: SessionId,
    ) -> Result<TerminalHandoff, crate::CoreError> {
        let _lease = self
            .begin_turn(session, &tokio_util::sync::CancellationToken::new())
            .await?
            .ok_or(crate::CoreError::Cancelled)?;
        Ok(self.terminal_handoff_claimed(session, None).await)
    }

    /// The caller holds the turn lease through handoff publication.
    pub(crate) async fn terminal_handoff_claimed(
        &self,
        session: SessionId,
        actor_claim: Option<&hya_proto::ActorClaim>,
    ) -> TerminalHandoff {
        let Ok(projection) = self.read_projection_shared(session).await else {
            return TerminalHandoff {
                generation: 1,
                doc: degraded_handoff_doc(None, &[]),
                degraded: true,
            };
        };
        let previous = hya_tool::extension::handoff_projection(&projection);
        let generation = previous
            .as_ref()
            .map_or(1, |handoff| handoff.generation.saturating_add(1));
        let previous_doc = previous.map(|handoff| handoff.doc);
        let degraded_doc = prepare_degraded_handoff_doc(Some(&projection));
        let summarizer = self.summarizer.clone();
        let messages = summarizer
            .as_ref()
            .and_then(|_| summary_messages(&projection).ok());
        drop(projection);
        if let Some(summarizer) = summarizer
            && let Some(messages) = messages
        {
            let usage = UsageCollector::default();
            let options = SummarizeOptions {
                previous_summary: previous_doc,
                max_output_tokens: Some(self.handoff_options.max_output_tokens),
                usage: Some(usage.clone()),
                ..SummarizeOptions::default()
            };
            let written = summarizer.summarize(&messages, options).await;
            // The terminal handoff is a summarizer call billed to this session.
            self.record_side_call_usage(actor_claim, session, UsagePurpose::Compaction, &usage)
                .await;
            if let Ok(doc) = written {
                return TerminalHandoff {
                    generation,
                    doc,
                    degraded: false,
                };
            }
        }
        TerminalHandoff {
            generation,
            doc: degraded_doc(&self.unread_handoff_mail(session).await),
            degraded: true,
        }
    }
}

/// The ended episode contributes goal/state/pending; only durable files and
/// decisions carry forward from the previous generation.
fn degraded_handoff_doc(projection: Option<&Projection>, unread: &[String]) -> String {
    prepare_degraded_handoff_doc(projection)(unread)
}

// Keep only fallback text while the model runs; unread mail is still read afterwards.
fn prepare_degraded_handoff_doc(
    projection: Option<&Projection>,
) -> impl FnOnce(&[String]) -> String + use<> {
    use hya_proto::{PartProjection, Role};
    let messages = projection.map(episode_messages).unwrap_or_default();
    let texts: Vec<_> = messages
        .iter()
        .map(|message| {
            let text = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    PartProjection::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            (message.role, text)
        })
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    let carry = |number: u8| {
        let Some(previous) = projection.and_then(hya_tool::extension::handoff_projection) else {
            return "unknown".into();
        };
        let prefix = format!("{number}. ");
        let next = format!("\n{}. ", number + 1);
        let Some(start) = previous.doc.find(&prefix) else {
            return "unknown".into();
        };
        let section = previous.doc[start..]
            .split(&next)
            .next()
            .unwrap_or_default();
        let content = section
            .split_once(" - ")
            .map_or(section, |(_, text)| text)
            .trim();
        format!("(from generation {}) {content}", previous.generation)
    };
    let files = carry(3);
    let decisions = carry(4);
    let goal = texts
        .iter()
        .find(|(role, _)| *role == Role::User)
        .map(|(_, text)| text.clone());
    let current = texts
        .iter()
        .rev()
        .find(|(role, _)| *role == Role::Assistant)
        .map_or_else(
            || "no work done in this episode".into(),
            |(_, text)| text.clone(),
        );
    let pending = texts
        .iter()
        .rev()
        .find(|(role, _)| *role == Role::User)
        .map_or_else(|| "none".into(), |(_, text)| text.clone());
    move |unread| {
        let goal = goal
            .as_deref()
            .or_else(|| unread.first().map(String::as_str))
            .unwrap_or("no work requested in this episode");
        let pending = if unread.is_empty() {
            pending
        } else {
            unread.join("\n")
        };
        format!(
            "1. Goal - {goal}\n2. Current state - {current}\n3. Files and code - {}\n4. Decisions - {}\n5. Pending tasks - {pending}\n6. Next step - none\n",
            files, decisions
        )
    }
}

impl SessionEngine {
    async fn unread_handoff_mail(&self, session: SessionId) -> Vec<String> {
        let Ok(root) = self.team_root(session).await else {
            return Vec::new();
        };
        let Ok(projection) = self.read_projection_shared(root).await else {
            return Vec::new();
        };
        let team = hya_tool::extension::team_projection(&projection);
        let Some(entry) = team.roster.values().find(|entry| entry.session == session) else {
            return Vec::new();
        };
        team.unread_incoming_mail(&entry.handle, entry.resident_cursor)
            .map(|mail| format!("[mail from {}] {}", mail.from, mail.body))
            .collect()
    }

    /// Deterministic degraded handoff, bypassing the summarizer entirely
    /// (`archive`, drain, and budget-kill paths must never wait on a model call).
    pub async fn degraded_terminal_handoff(&self, session: SessionId) -> TerminalHandoff {
        match self.read_projection_shared(session).await {
            Ok(projection) => {
                let generation = hya_tool::extension::handoff_projection(&projection)
                    .map_or(1, |handoff| handoff.generation.saturating_add(1));
                TerminalHandoff {
                    generation,
                    doc: degraded_handoff_doc(
                        Some(&projection),
                        &self.unread_handoff_mail(session).await,
                    ),
                    degraded: true,
                }
            }
            Err(_) => TerminalHandoff {
                generation: 1,
                doc: degraded_handoff_doc(None, &[]),
                degraded: true,
            },
        }
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
        let out = request_messages(&msgs, &options);
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
