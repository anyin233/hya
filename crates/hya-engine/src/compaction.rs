//! The host's side of context compaction. The ladder itself is the
//! `hya/compaction` internal bundle; the host keeps the settings it hands that
//! bundle ([`CompactionConfig`]) and the summarizer it calls back into
//! ([`Summarizer`]), which the terminal handoff also uses.

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
    /// Write a handoff document instead of a summary.
    ///
    /// A handoff call sees the transcript *verbatim* plus one trailing handoff
    /// prompt (see [`handoff_request_messages`]), rather than the rendered
    /// single-message serialization a summary gets: the document must describe
    /// where the session stands — including recent turns — for an agent taking
    /// over, and the request shape it rides matches the live conversation so a
    /// cache-capable provider can reuse the prefix.
    pub handoff: bool,
    /// Terminal handoff variant (ADR-0015): state only, no narrative.
    ///
    /// Selects [`STATE_HANDOFF_TEMPLATE`] over [`HANDOFF_TEMPLATE`] in
    /// [`handoff_request_messages`]. The terminal document is the only state an
    /// archived agent carries into its next episode, so it records what *is*,
    /// never how the session got there.
    pub state_only: bool,
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

/// Thresholds for when and how aggressively to compact a transcript. The
/// host passes them to the `hya/compaction` bundle as JSON.
#[derive(Clone, Copy, Debug)]
pub struct CompactionConfig {
    /// Fallback token threshold, used when the route advertises no window.
    pub token_threshold: usize,
    /// Number of recent messages retained unsummarized.
    pub keep_recent: usize,
    /// Share of the model's advertised context window at which to compact.
    ///
    /// Ignored when the route advertises no window, or when the value is outside
    /// `(0.0, 1.0]` — a nonsense fraction falls back to `token_threshold` rather
    /// than being trusted.
    pub context_fraction: f32,
    /// Tokens held back from the advertised window for the model's reply.
    ///
    /// Bounds the trigger independently of `context_fraction`: a generous
    /// fraction on a large window can still leave less headroom than the reply
    /// needs, and the tighter of the two bounds wins.
    pub reserve_tokens: usize,
    /// Output cap for the summarizer call that folds the transcript prefix.
    pub summary_max_tokens: u32,
    /// Order the five mechanisms are tried in when a turn crosses the threshold.
    ///
    /// A permutation of every rung (see [`CompactionRung::DEFAULT_ORDER`]), so
    /// reordering can never silently drop a mechanism. Parsed from
    /// oh-my-pi-compatible wire names by [`parse_method_order`].
    pub method_order: [CompactionRung; 5],
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            token_threshold: 100_000,
            keep_recent: 6,
            context_fraction: 0.75,
            reserve_tokens: 16_384,
            summary_max_tokens: 4_096,
            method_order: CompactionRung::DEFAULT_ORDER,
        }
    }
}

/// One of the five built-in context-reduction mechanisms (oh-my-pi parity).
///
/// The `hya/compaction` bundle walks the configured order (see
/// [`CompactionConfig::method_order`]) and stops at the first rung that brings
/// the transcript under the threshold, so an expensive reduction is only paid
/// for when the cheaper ones were not enough. An unavailable or failed rung
/// advances to the next one.
///
/// Each rung's wire name matches oh-my-pi's `compaction.methodOrder` exactly:
///
/// 1. [`SpillToolOutputs`] (`shake`) moves output bodies out of the request and
///    leaves a handle behind. Every call, input, and reasoning step survives
///    and the body stays retrievable — nothing is destroyed, it costs a fetch
///    to read.
/// 2. [`ProviderCompact`] (`remote`) lets the route fold its own window,
///    keeping whatever internal fidelity it chooses.
/// 3. [`Summarize`] (`soft`) folds the transcript prefix into a structured,
///    incrementally anchored summary — hya's primary fold.
/// 4. [`SnapCompact`] (`snapcompact`) replaces the folded prefix with a local
///    deterministic dense archive — no model call at all, so it also works
///    where no summarizer is wired or a model call just failed.
/// 5. [`Handoff`] (`handoff`) has a model write a handoff document over the
///    verbatim transcript, committed as the compaction summary — the hardest
///    fold, for taking a session over rather than continuing it.
///
/// The default order keeps hya's verified spill-first ladder intact and adds
/// the new mechanisms as escalation; oh-my-pi's own default
/// (`remote, snapcompact, handoff, shake, soft`) is one
/// [`parse_method_order`] away.
///
/// [`SpillToolOutputs`]: CompactionRung::SpillToolOutputs
/// [`ProviderCompact`]: CompactionRung::ProviderCompact
/// [`SnapCompact`]: CompactionRung::SnapCompact
/// [`Handoff`]: CompactionRung::Handoff
/// [`Summarize`]: CompactionRung::Summarize
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactionRung {
    /// Move stale tool-output bodies to durable storage, leaving handles.
    SpillToolOutputs,
    /// Ask the route to fold its own context window.
    ProviderCompact,
    /// Replace the foldable prefix with a local deterministic dense archive.
    SnapCompact,
    /// Fold the prefix behind a model-written handoff document.
    Handoff,
    /// Fold the transcript prefix into a structured summary.
    Summarize,
}

impl CompactionRung {
    /// The default escalation order: hya's verified spill-first ladder, with
    /// the new mechanisms as escalation — the model-free archive before the
    /// final model call, so a session with no summarizer still folds.
    pub const DEFAULT_ORDER: [Self; 5] = [
        Self::SpillToolOutputs,
        Self::ProviderCompact,
        Self::Summarize,
        Self::SnapCompact,
        Self::Handoff,
    ];

    /// Every rung, in declaration order (the canonical permutation).
    pub const ALL: [Self; 5] = [
        Self::SpillToolOutputs,
        Self::ProviderCompact,
        Self::SnapCompact,
        Self::Handoff,
        Self::Summarize,
    ];

    /// The oh-my-pi `compaction.methodOrder` name for this rung.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::SpillToolOutputs => "shake",
            Self::ProviderCompact => "remote",
            Self::SnapCompact => "snapcompact",
            Self::Handoff => "handoff",
            Self::Summarize => "soft",
        }
    }

    /// Parse one oh-my-pi method name.
    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rung| rung.wire_name() == name)
    }
}

/// Parse a configured method order into a permutation of all five rungs.
///
/// oh-my-pi names are accepted verbatim, but unlike omp's filter-and-drop
/// policy the result must name every mechanism exactly once: a list with a
/// duplicate or a typo means one rung is silently missing from the ladder, so
/// [`None`] is returned and callers keep their default order rather than
/// trusting a broken one.
#[must_use]
pub fn parse_method_order(names: &[&str]) -> Option<[CompactionRung; 5]> {
    if names.len() != CompactionRung::ALL.len() {
        return None;
    }
    let mut order = [CompactionRung::SpillToolOutputs; 5];
    for (slot, name) in order.iter_mut().zip(names) {
        *slot = CompactionRung::from_wire_name(name.trim())?;
    }
    // A repeated rung means another one is missing from the ladder.
    for i in 0..order.len() {
        if order[i + 1..].contains(&order[i]) {
            return None;
        }
    }
    Some(order)
}

/// Produces a summary string for older transcript segments.
///
/// **Contract:** Called only with the messages being folded. Must not write the
/// session store. Errors abort compaction and leave the transcript unchanged.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Summarize `messages` into a single string for a system summary message.
    ///
    /// # Errors
    /// Propagate provider failures as [`CoreError`].
    async fn summarize(
        &self,
        messages: &[Message],
        options: SummarizeOptions,
    ) -> Result<String, CoreError>;
}

/// Share of a byte budget kept from the head of a payload.
const HEAD_RATIO: f32 = 0.6;

/// Largest char-boundary-safe prefix of at most `max` bytes.
fn floor_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Smallest char-boundary-safe suffix start of at least `min` bytes from the end.
fn ceil_boundary(text: &str, min: usize) -> usize {
    let mut start = text.len().saturating_sub(min);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// Keep the head and tail of `text` under a byte budget, saying how much was
/// dropped. Errors and final state live at the ends of a tool output, not its
/// middle, so a head+tail split preserves more usable signal than a prefix cut.
fn head_tail_budgeted(text: &str, max: usize, head_ratio: f32) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let head = ((max as f32) * head_ratio) as usize;
    let head = head.clamp(1, max.saturating_sub(1));
    let tail = max - head;
    let head_end = floor_boundary(text, head);
    let tail_start = ceil_boundary(text, tail);
    let omitted = text.len() - head_end - (text.len() - tail_start);
    format!(
        "{}…[{} chars omitted]…{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

/// Section structure of a handoff document.
///
/// A handoff differs from a summary in audience: it is written for an agent
/// taking over the session exactly where it stands, so it leads with goal and
/// current state rather than narrative, and the model writing it sees the
/// verbatim recent turns instead of a rendered serialization.
const HANDOFF_TEMPLATE: &str = "\
Write a handoff document for an agent taking over this session exactly where \
it stands, under exactly these headings, keeping every heading even when its \
section is empty:

1. Goal - what the user asked for, in their terms.
2. Current state - what exists and works right now.
3. Files and code - exact paths touched, and what changed or matters in each.
4. Decisions - choices made and why, including what was ruled out.
5. Errors and fixes - failures hit, their causes, and how they were resolved.
6. Pending tasks - work explicitly requested and not yet done.
7. Next step - the single next action, or `none` when the work is complete.

Preserve exact file paths, identifiers, signatures, and command lines. Prefer \
terse bullets over paragraphs. The reader sees the recent turns only through \
this document.";

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

/// Request messages for a handoff call: the transcript verbatim, plus exactly
/// one trailing prompt.
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
    let template = if options.state_only {
        STATE_HANDOFF_TEMPLATE
    } else {
        HANDOFF_TEMPLATE
    };
    let _ = write!(prompt, "{template}");
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

/// Bytes of any single rendered payload the summarizer is shown.
///
/// One 50KB tool result must not crowd fifty turns of history out of the
/// summarizer's own window, and a summary needs the *shape* of a result — what
/// ran, against what, whether it worked — far more than its every byte.
const SUMMARY_PART_BUDGET: usize = 2_000;

/// Truncate on a char boundary, saying how much was dropped.
///
/// The byte count matters: it tells the model the difference between a result
/// it has seen in full and one it has seen the beginning of.
fn budgeted(text: &str) -> String {
    if text.len() <= SUMMARY_PART_BUDGET {
        return text.to_string();
    }
    let mut end = SUMMARY_PART_BUDGET;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}… [{} more bytes]",
        &text[..end],
        text.len().saturating_sub(end)
    )
}

/// [`budgeted`] keeping both ends: a long reasoning reaches its conclusion,
/// and a tool output its verdict, at the end.
fn ends_budgeted(text: &str) -> String {
    head_tail_budgeted(text, SUMMARY_PART_BUDGET, HEAD_RATIO)
}

/// Render a JSON value as the model would have seen it.
fn value_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), ToString::to_string)
}

/// Render every part of a message, not only its prose.
///
/// A coding session is mostly tool activity. Rendering only `Part::Text` showed
/// the summarizer what the agent *said* and hid what it *did*, so summaries
/// described intentions and lost the commands, paths, and results that make a
/// compacted session resumable.
fn render_parts(out: &mut String, parts: &[Part]) {
    for part in parts {
        match part {
            Part::Text { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(out, "{}", budgeted(text));
            }
            Part::Reasoning { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(out, "[reasoning] {}", ends_budgeted(text));
            }
            Part::Media {
                media_type,
                data,
                filename,
                ..
            } => {
                let name = filename.as_deref().unwrap_or("attachment");
                let _ = writeln!(out, "[media {name} ({media_type}), {} bytes]", data.len());
            }
            Part::Tool { name, state, .. } => {
                let tool = name.as_str();
                match state {
                    hya_proto::ToolPartState::Pending { input }
                    | hya_proto::ToolPartState::Running { input } => {
                        let _ = writeln!(
                            out,
                            "[tool {tool}] input: {} (did not finish)",
                            budgeted(&input.to_string())
                        );
                    }
                    hya_proto::ToolPartState::Completed { input, output, .. } => {
                        let _ =
                            writeln!(out, "[tool {tool}] input: {}", budgeted(&input.to_string()));
                        let _ = writeln!(
                            out,
                            "[tool {tool}] output: {}",
                            ends_budgeted(&value_text(output))
                        );
                    }
                    hya_proto::ToolPartState::Error { input, message, .. } => {
                        let _ =
                            writeln!(out, "[tool {tool}] input: {}", budgeted(&input.to_string()));
                        let _ = writeln!(out, "[tool {tool}] failed: {}", ends_budgeted(message));
                    }
                }
            }
            Part::Text { .. } | Part::Reasoning { .. } => {}
        }
    }
}

fn render_for_summary(messages: &[Message]) -> String {
    let mut s = String::new();
    for m in messages {
        match m {
            Message::User { parts, .. } => {
                let _ = writeln!(s, "[user]");
                render_parts(&mut s, parts);
            }
            Message::Assistant { parts, .. } => {
                let _ = writeln!(s, "[assistant]");
                render_parts(&mut s, parts);
            }
            Message::System { content, .. } => {
                let _ = writeln!(s, "[system] {}", budgeted(content));
            }
        }
    }
    s
}

/// Output cap used when the caller names none.
///
/// Matches `CompactionConfig::summary_max_tokens`; the two agree so a summary
/// is not silently cut short on the paths that do not thread config through.
const DEFAULT_SUMMARY_MAX_TOKENS: u32 = 4_096;

/// Section structure the summarizer must fill.
///
/// `compaction.md` instructs the model to "follow the exact output structure
/// requested by the user prompt" — this is that structure. Naming the sections
/// is what makes a compacted session resumable: an unstructured précis
/// reliably keeps the narrative and drops the paths, signatures, and pending
/// work that the next turn actually needs.
const SUMMARY_TEMPLATE: &str = "\
Summarize the conversation below under exactly these headings, keeping every \
heading even when its section is empty:

1. Primary request and intent - what the user asked for, in their terms.
2. Key technical concepts - frameworks, invariants, and decisions in play.
3. Files and code touched - exact paths, and what changed or matters in each.
4. Errors and fixes - failures hit, their causes, and how they were resolved.
5. Problem solving - approaches tried, what worked, what was ruled out and why.
6. Pending tasks - work explicitly requested and not yet done.
7. Current work - what was in progress at the moment this summary was taken.
8. Next step - the single next action, or `none` when the work is complete.

Preserve exact file paths, identifiers, signatures, and command lines. Prefer \
terse bullets over paragraphs.";

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
        // A handoff rides the transcript itself plus one trailing prompt, so
        // the model reads the session verbatim (and a cache-capable provider
        // reuses the prefix). A summary reads the rendered serialization.
        let request_messages = if options.handoff {
            handoff_request_messages(messages, &options)
        } else {
            let transcript = render_for_summary(messages);
            let mut prompt = String::new();
            // Anchoring block first: the model reads it as the state to update,
            // and the conversation below as the delta to fold into it.
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
            let _ = write!(
                prompt,
                "{SUMMARY_TEMPLATE}\n\n<conversation>\n{transcript}</conversation>\n"
            );
            vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: prompt,
                }],
            }]
        };
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

    fn assistant_with_tool(output: &str) -> Message {
        use hya_proto::{ToolCallId, ToolName, ToolPartState};
        Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: vec![Part::Tool {
                id: PartId::new(),
                call_id: ToolCallId::new(),
                name: ToolName::new("find"),
                state: ToolPartState::Completed {
                    input: serde_json::json!({"pattern": "*.rs"}),
                    output: serde_json::Value::String(output.to_string()),
                    time_ms: 7,
                },
            }],
            finish: None,
            tokens: None,
            last_round: None,
        }
    }
    fn assistant(parts: Vec<Part>) -> Message {
        Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts,
            finish: None,
            tokens: None,
            last_round: None,
        }
    }

    /// The five oh-my-pi mechanisms ship built in, wire-named exactly as omp
    /// names them; the default order preserves hya's verified ladder and adds
    /// the new mechanisms as escalation.
    #[test]
    fn method_order_defaults_to_the_documented_ladder() {
        assert_eq!(
            CompactionConfig::default().method_order,
            [
                CompactionRung::SpillToolOutputs,
                CompactionRung::ProviderCompact,
                CompactionRung::Summarize,
                CompactionRung::SnapCompact,
                CompactionRung::Handoff,
            ]
        );
        for (rung, wire) in [
            (CompactionRung::SpillToolOutputs, "shake"),
            (CompactionRung::ProviderCompact, "remote"),
            (CompactionRung::SnapCompact, "snapcompact"),
            (CompactionRung::Handoff, "handoff"),
            (CompactionRung::Summarize, "soft"),
        ] {
            assert_eq!(rung.wire_name(), wire);
            assert_eq!(CompactionRung::from_wire_name(wire), Some(rung));
        }
    }

    /// oh-my-pi's own default order (`remote, snapcompact, handoff, shake,
    /// soft`) is one parse away, so parity is a config line, not a patch.
    #[test]
    fn method_order_parses_the_omp_default() {
        assert_eq!(
            parse_method_order(&["remote", "snapcompact", "handoff", "shake", "soft"]),
            Some([
                CompactionRung::ProviderCompact,
                CompactionRung::SnapCompact,
                CompactionRung::Handoff,
                CompactionRung::SpillToolOutputs,
                CompactionRung::Summarize,
            ])
        );
    }

    /// Anything that is not a permutation of the five mechanisms is rejected,
    /// so a typo cannot silently drop a mechanism from the ladder.
    #[test]
    fn method_order_rejects_non_permutations() {
        assert_eq!(parse_method_order(&[]), None);
        assert_eq!(parse_method_order(&["shake"]), None);
        assert_eq!(
            parse_method_order(&["shake", "remote", "snapcompact", "handoff", "handoff"]),
            None,
            "a duplicate means one mechanism is missing"
        );
        assert_eq!(
            parse_method_order(&["shake", "remote", "snapcompact", "handoff", "xd"]),
            None,
            "unknown names are rejected"
        );
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
        assert!(text.contains("handoff document"), "{text}");
    }

    /// A coding session is mostly tool activity. A summarizer shown only prose
    /// learns what the agent *said* and not what it *did*, so its summaries
    /// describe intentions and lose the commands, paths, and results that make
    /// a compacted session resumable.
    #[test]
    fn summarizer_input_includes_tool_activity() {
        let msgs = vec![
            user("find the config loader"),
            assistant_with_tool("crates/hya-app/src/config.rs"),
        ];
        let rendered = render_for_summary(&msgs);
        assert!(rendered.contains("find the config loader"));
        assert!(rendered.contains("[tool find]"), "{rendered}");
        assert!(
            rendered.contains("*.rs"),
            "the tool input must survive: {rendered}"
        );
        assert!(
            rendered.contains("crates/hya-app/src/config.rs"),
            "the tool output must survive: {rendered}"
        );
    }

    /// Including tool output must not mean one 100KB result crowds every other
    /// turn out of the summarizer's own context window.
    #[test]
    fn summarizer_input_budgets_one_huge_payload() {
        let msgs = vec![assistant_with_tool(&"x".repeat(100_000))];
        let rendered = render_for_summary(&msgs);
        assert!(
            rendered.len() < 10_000,
            "one huge tool output must not fill the summarizer window: {} bytes",
            rendered.len()
        );
        assert!(
            rendered.contains("chars omitted]"),
            "truncation must be stated so the model knows it saw a part"
        );
    }

    /// A long reasoning reaches its conclusion at the end; the summarizer must
    /// see that end, not only the opening.
    #[test]
    fn summarizer_input_keeps_both_ends_of_long_reasoning() {
        let text = format!("OPENING {} CONCLUSION", "x".repeat(100_000));
        let rendered = render_for_summary(&[assistant(vec![Part::Reasoning {
            id: PartId::new(),
            text,
            provider_data: None,
        }])]);
        assert!(rendered.contains("OPENING"), "{rendered}");
        assert!(rendered.contains("CONCLUSION"), "{rendered}");
        assert!(rendered.len() < 10_000);
    }
}
