use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use hya_proto::{Event, Message, MessageId, ModelRef, Part, PartId, SessionId};
use hya_provider::{CompletionRequest, ProviderRouter, ReasoningEffort};

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
}

/// Thresholds for when and how aggressively to compact a transcript.
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

/// Smallest threshold [`resolved_threshold`] will ever return.
///
/// A threshold near zero would compact on every single turn, which is worse than
/// not compacting at all.
pub const MIN_RESOLVED_THRESHOLD: usize = 1_000;

/// Token threshold for this turn: the tighter of a share of the model's
/// advertised context window and that window minus the response reserve.
///
/// `max_context` of `None` or `0` means the route advertises nothing, so the
/// configured threshold stands and behaviour matches the pre-window default.
#[must_use]
pub fn resolved_threshold(cfg: &CompactionConfig, max_context: Option<u32>) -> usize {
    let Some(window) = max_context.filter(|w| *w > 0) else {
        return cfg.token_threshold;
    };
    if !(cfg.context_fraction > 0.0 && cfg.context_fraction <= 1.0) {
        return cfg.token_threshold;
    }
    let scaled = f64::from(window) * f64::from(cfg.context_fraction);
    // `as` on a finite non-negative f64 saturates at usize::MAX here; the value
    // is bounded by u32::MAX anyway.
    let scaled = scaled as usize;
    let window = usize::try_from(window).unwrap_or(usize::MAX);
    let reserved = window.saturating_sub(cfg.reserve_tokens);
    scaled.min(reserved).max(MIN_RESOLVED_THRESHOLD)
}

fn message_text_len(m: &Message) -> usize {
    match m {
        Message::User { parts, .. } | Message::Assistant { parts, .. } => {
            parts.iter().map(part_len).sum()
        }
        Message::System { content, .. } => content.len(),
    }
}

/// Approximate serialized size of a part for compaction thresholds.
///
/// Tool-heavy turns historically never tripped compaction because only text
/// was counted; include reasoning + tool I/O so subagent explore loops compact.
fn part_len(part: &Part) -> usize {
    match part {
        Part::Text { text, .. } => text.len(),
        Part::Reasoning {
            text,
            provider_data,
            ..
        } => {
            text.len()
                + provider_data
                    .as_ref()
                    .map(|v| v.to_string().len())
                    .unwrap_or(0)
        }
        Part::Media { data, .. } => data.len(),
        Part::Tool { name, state, .. } => {
            name.as_str().len()
                + match state {
                    hya_proto::ToolPartState::Pending { input }
                    | hya_proto::ToolPartState::Running { input } => input.to_string().len(),
                    hya_proto::ToolPartState::Completed { input, output, .. } => {
                        input.to_string().len() + value_text_len(output)
                    }
                    hya_proto::ToolPartState::Error {
                        input,
                        message,
                        value,
                        ..
                    } => {
                        input.to_string().len()
                            + message.len()
                            + value.as_ref().map(value_text_len).unwrap_or(0)
                    }
                }
        }
    }
}

fn value_text_len(value: &serde_json::Value) -> usize {
    match value.as_str() {
        Some(s) => s.len(),
        None => value.to_string().len(),
    }
}

/// Rough token estimate: total part character length / 4.
#[must_use]
pub fn estimate_tokens(messages: &[Message]) -> usize {
    let chars: usize = messages.iter().map(message_text_len).sum();
    chars / 4
}

/// Provider-reported prompt size, plus an estimate of everything appended since.
///
/// The most recent assistant message that reported usage tells us exactly how
/// many tokens the provider counted for that request; only the messages after it
/// still need estimating. Returns `None` when no usage was ever reported (for
/// example a route with `usage_reporting: false`), so callers fall back to
/// [`estimate_tokens`] over the whole transcript.
///
/// Window occupancy counts `input + cache_read`: cached prompt tokens still take
/// up the window, and providers disagree on whether `input` already includes
/// them. Summing can only over-count, which fails safe — compacting slightly
/// early rather than overflowing.
#[must_use]
pub fn measured_tokens(messages: &[Message]) -> Option<usize> {
    let (index, usage) = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, m)| match m {
            Message::Assistant {
                tokens: Some(usage),
                ..
            } if !usage.is_zero() => Some((i, usage)),
            _ => None,
        })?;
    let measured =
        usize::try_from(usage.input.saturating_add(usage.cache_read)).unwrap_or(usize::MAX);
    let appended = estimate_tokens(messages.get(index + 1..).unwrap_or(&[]));
    Some(measured.saturating_add(appended))
}

/// Best available token count for `messages`: measured when the provider has
/// reported usage, estimated otherwise.
#[must_use]
pub fn tokens_in_use(messages: &[Message]) -> usize {
    measured_tokens(messages).unwrap_or_else(|| estimate_tokens(messages))
}

/// Whether `messages` exceeds keep_recent and the configured flat threshold.
///
/// Uses `cfg.token_threshold` directly. Callers that know the active model's
/// window should prefer [`needs_compaction_at`] with [`resolved_threshold`].
#[must_use]
pub fn needs_compaction(messages: &[Message], cfg: &CompactionConfig) -> bool {
    needs_compaction_at(messages, cfg, cfg.token_threshold)
}

/// Whether `messages` exceeds keep_recent and an explicit token `threshold`.
#[must_use]
pub fn needs_compaction_at(messages: &[Message], cfg: &CompactionConfig, threshold: usize) -> bool {
    messages.len() > cfg.keep_recent && tokens_in_use(messages) > threshold
}

/// One of the five built-in context-reduction mechanisms (oh-my-pi parity).
///
/// The turn loop walks the configured order (see
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

/// Replaces a dropped tool output when no sink preserved the body.
///
/// This is the lossy path: the bytes are gone from the request and re-running
/// the tool is the model's only recourse, which costs what it cost the first
/// time. Taken only when no sink is wired.
const EVICTED_OUTPUT_NOTICE: &str = "[tool output evicted to fit the context window; \
                                     re-run the tool if you still need it]";

/// Opening of the notice left when a body was preserved rather than dropped.
///
/// Matched as a prefix so a later pass recognizes its own work.
const SPILLED_OUTPUT_PREFIX: &str = "[tool output moved to ";

/// Where an evicted tool-output body is preserved so the model can fetch it back.
///
/// With a sink, eviction is a *move* rather than a drop: the body lands in
/// durable storage and the transcript keeps a handle that retrieves it. Without
/// one the output is simply gone, which is the difference between the ladder's
/// cheapest rung being recoverable and being merely cheap.
pub trait EvictionSink: Send + Sync {
    /// Persist `body`, produced by `tool`, and return the handle that reads it back.
    ///
    /// Returning `None` falls back to the lossy notice. A sink that cannot store
    /// one particular body must not fail the compaction that is trying to keep
    /// the turn inside its window.
    fn spill(&self, tool: &str, body: &str) -> Option<String>;
}

/// Notice left in place of an output whose body moved to `handle`.
fn spilled_output_notice(handle: &str) -> String {
    format!(
        "{SPILLED_OUTPUT_PREFIX}{handle} to fit the context window; \
         read that handle to retrieve it]"
    )
}

/// Whether `output` is a notice a previous eviction pass already left behind.
///
/// Both shapes are recognized so a repeat pass is a no-op and the reported
/// count reflects real work rather than re-evicting a notice.
fn is_eviction_notice(output: &serde_json::Value) -> bool {
    output.as_str().is_some_and(|text| {
        text == EVICTED_OUTPUT_NOTICE || text.starts_with(SPILLED_OUTPUT_PREFIX)
    })
}

/// Drop stale completed tool outputs from `messages`, keeping calls and inputs.
///
/// Returns how many parts were evicted. Messages in the most recent
/// `keep_recent` are never touched, so the model keeps full fidelity on what it
/// just did.
///
/// **Request-local.** Callers pass a transcript built for one provider request;
/// the event log is untouched, so the full output stays recoverable offline.
///
/// This is tried before summarizing because tool output dominates a tool-heavy
/// transcript, and losing it costs the model far less than folding whole turns
/// into prose: every call, its input, and all reasoning survive.
pub fn evict_stale_tool_outputs(
    messages: &mut [Message],
    keep_recent: usize,
    sink: Option<&dyn EvictionSink>,
) -> u32 {
    let cutoff = messages.len().saturating_sub(keep_recent);
    let mut evicted = 0;
    for message in messages.iter_mut().take(cutoff) {
        let (Message::Assistant { parts, .. } | Message::User { parts, .. }) = message else {
            continue;
        };
        for part in parts.iter_mut() {
            let Part::Tool { name, state, .. } = part else {
                continue;
            };
            let hya_proto::ToolPartState::Completed {
                input,
                output,
                time_ms,
            } = state
            else {
                continue;
            };
            // Already evicted: skip so a repeat pass is idempotent and the count
            // reflects real work.
            if is_eviction_notice(output) {
                continue;
            }
            // Prefer moving the body to dropping it. The sink is handed the
            // rendered output because an artifact is bytes and a tool result is
            // only sometimes a string.
            let notice = sink
                .and_then(|sink| sink.spill(name.as_str(), &value_text(output)))
                .map_or_else(
                    || EVICTED_OUTPUT_NOTICE.to_string(),
                    |handle| spilled_output_notice(&handle),
                );
            *state = hya_proto::ToolPartState::Completed {
                input: input.clone(),
                output: serde_json::Value::String(notice),
                time_ms: *time_ms,
            };
            evicted += 1;
        }
    }
    evicted
}

/// The anchored summary already present in `messages`, if any.
///
/// Compaction is incremental: the summary a session already carries is the
/// baseline the next one updates. Re-summarizing a summary from scratch is how
/// detail decays across a long session, because each pass paraphrases the
/// previous pass's prose rather than the original events.
///
/// Returns `None` for a window folded by the provider's own compaction, whose
/// body is serialized response items rather than prose and means nothing to a
/// summarizer prompt.
#[must_use]
pub fn previous_summary(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|m| {
        let Message::System { content, .. } = m else {
            return None;
        };
        let rest = content
            .strip_prefix(hya_provider::COMPACT_CONTEXT_MARKER)?
            .trim_start();
        if rest.starts_with(hya_provider::RESPONSES_COMPACT_ITEMS_MARKER) {
            return None;
        }
        (!rest.is_empty()).then(|| rest.to_string())
    })
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

/// What a local compaction folded, and the summary produced for it.
///
/// Carries the range so the caller can persist a `ContextCompacted` record
/// pointing at the folded messages instead of copying them.
#[derive(Clone, Debug)]
pub struct CompactionPlan {
    /// Summary text produced for the folded prefix.
    pub summary: String,
    /// First message folded.
    pub from_message: MessageId,
    /// Last message folded.
    pub to_message: MessageId,
    /// Number of messages folded.
    pub folded_count: u32,
}

/// Summarize the foldable prefix of `messages`, or `None` when under threshold.
///
/// Folds `messages[..len - keep_recent]`, leaving the most recent `keep_recent`
/// untouched. Callers persist the result; this function performs no store writes.
///
/// # Errors
/// Propagates summarizer failures.
pub async fn plan_compaction(
    messages: &[Message],
    cfg: &CompactionConfig,
    summarizer: &dyn Summarizer,
    options: SummarizeOptions,
) -> Result<Option<CompactionPlan>, CoreError> {
    plan_compaction_at(messages, cfg, cfg.token_threshold, summarizer, options).await
}

/// [`plan_compaction`] against an explicit token `threshold`.
///
/// The turn loop passes the window-scaled threshold so this cannot disagree with
/// the decision that got us here.
///
/// # Errors
/// Propagates summarizer failures.
pub async fn plan_compaction_at(
    messages: &[Message],
    cfg: &CompactionConfig,
    threshold: usize,
    summarizer: &dyn Summarizer,
    options: SummarizeOptions,
) -> Result<Option<CompactionPlan>, CoreError> {
    if !needs_compaction_at(messages, cfg, threshold) {
        return Ok(None);
    }
    fold_prefix(messages, cfg, summarizer, options).await
}

/// Summarize the foldable prefix unconditionally, without re-checking a threshold.
///
/// For callers that already decided to compact. Re-deriving the decision here
/// would be wrong after a request-local edit such as tool-output eviction: the
/// provider-measured token count still describes the pre-edit transcript.
///
/// # Errors
/// Propagates summarizer failures.
pub async fn fold_prefix(
    messages: &[Message],
    cfg: &CompactionConfig,
    summarizer: &dyn Summarizer,
    options: SummarizeOptions,
) -> Result<Option<CompactionPlan>, CoreError> {
    if messages.len() <= cfg.keep_recent {
        return Ok(None);
    }
    let split = messages.len() - cfg.keep_recent;
    let older = &messages[..split];
    // `needs_compaction` guarantees `split >= 1`; stay panic-free regardless.
    let (Some(first), Some(last)) = (older.first(), older.last()) else {
        return Ok(None);
    };
    let from_message = first.id();
    let to_message = last.id();
    let summary = summarizer.summarize(older, options).await?;
    Ok(Some(CompactionPlan {
        summary,
        from_message,
        to_message,
        folded_count: u32::try_from(split).unwrap_or(u32::MAX),
    }))
}

/// Max rendered characters of one tool result in a snapcompact archive.
///
/// oh-my-pi's `toolResultMaxChars` default: the ends of an output carry its
/// verdict and final state, so they are kept and the middle is dropped.
pub const SNAP_TOOL_RESULT_MAX_CHARS: usize = 2_000;
/// Share of [`SNAP_TOOL_RESULT_MAX_CHARS`] kept from the head of a payload.
const SNAP_HEAD_RATIO: f32 = 0.6;
/// Max rendered characters of one string value inside a tool-call input.
///
/// oh-my-pi's `toolArgMaxChars` default.
const SNAP_TOOL_ARG_MAX_CHARS: usize = 500;
/// Max rendered characters of a tool call's whole input object.
///
/// oh-my-pi's `toolCallMaxChars` default.
const SNAP_TOOL_CALL_MAX_CHARS: usize = 2_000;

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

/// Collapse runs of blanks to one blank line and runs of spaces/tabs to one.
///
/// An archive trades formatting for density: the words and structure survive,
/// the indentation that carried them does not.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = false;
    for line in text.lines() {
        let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            blank_run = true;
            continue;
        }
        if blank_run && !out.is_empty() {
            out.push('\n');
        }
        blank_run = false;
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&collapsed);
    }
    out
}

/// Cap every string leaf of a JSON value at [`SNAP_TOOL_ARG_MAX_CHARS`].
fn cap_json_strings(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) if s.len() > SNAP_TOOL_ARG_MAX_CHARS => {
            let end = floor_boundary(s, SNAP_TOOL_ARG_MAX_CHARS);
            serde_json::Value::String(format!("{}…", &s[..end]))
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(cap_json_strings).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_json_strings(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Render a tool-call input under the per-value and per-call budgets.
fn budget_tool_input(input: &serde_json::Value) -> String {
    let rendered = cap_json_strings(input).to_string();
    head_tail_budgeted(&rendered, SNAP_TOOL_CALL_MAX_CHARS, SNAP_HEAD_RATIO)
}

/// Serialize a transcript into the snapcompact archive: a local, deterministic,
/// model-free fold of the discarded history.
///
/// This is hya's `snapcompact` mechanism. oh-my-pi renders the same serialized
/// archive onto bitmap image frames for vision-capable models; hya commits the
/// dense text itself, which every route can read, with omp's serialization
/// budgets (head+tail tool results, capped call arguments, collapsed
/// whitespace) so one huge payload cannot crowd out the turns around it.
pub fn snapcompact_archive(messages: &[Message]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "[snapcompact archive of {} messages]", messages.len());
    for message in messages {
        match message {
            Message::User { parts, .. } => {
                let _ = writeln!(out, "[user]");
                snap_parts(&mut out, parts);
            }
            Message::Assistant { parts, .. } => {
                let _ = writeln!(out, "[assistant]");
                snap_parts(&mut out, parts);
            }
            Message::System { content, .. } => {
                let _ = writeln!(
                    out,
                    "[system] {}",
                    head_tail_budgeted(
                        &collapse_whitespace(content),
                        SNAP_TOOL_RESULT_MAX_CHARS,
                        SNAP_HEAD_RATIO
                    )
                );
            }
        }
    }
    out
}

fn snap_parts(out: &mut String, parts: &[Part]) {
    for part in parts {
        match part {
            Part::Text { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(
                    out,
                    "{}",
                    head_tail_budgeted(
                        &collapse_whitespace(text),
                        SNAP_TOOL_RESULT_MAX_CHARS,
                        SNAP_HEAD_RATIO
                    )
                );
            }
            Part::Reasoning { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(
                    out,
                    "[reasoning] {}",
                    head_tail_budgeted(
                        &collapse_whitespace(text),
                        SNAP_TOOL_RESULT_MAX_CHARS,
                        SNAP_HEAD_RATIO
                    )
                );
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
                            budget_tool_input(input)
                        );
                    }
                    hya_proto::ToolPartState::Completed { input, output, .. } => {
                        let _ = writeln!(out, "[tool {tool}] input: {}", budget_tool_input(input));
                        let _ = writeln!(
                            out,
                            "[tool {tool}] output: {}",
                            head_tail_budgeted(
                                &collapse_whitespace(&value_text(output)),
                                SNAP_TOOL_RESULT_MAX_CHARS,
                                SNAP_HEAD_RATIO
                            )
                        );
                    }
                    hya_proto::ToolPartState::Error { input, message, .. } => {
                        let _ = writeln!(out, "[tool {tool}] input: {}", budget_tool_input(input));
                        let _ = writeln!(
                            out,
                            "[tool {tool}] failed: {}",
                            head_tail_budgeted(
                                &collapse_whitespace(message),
                                SNAP_TOOL_RESULT_MAX_CHARS,
                                SNAP_HEAD_RATIO
                            )
                        );
                    }
                }
            }
            Part::Text { .. } | Part::Reasoning { .. } => {}
        }
    }
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
    let _ = write!(prompt, "{HANDOFF_TEMPLATE}");
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

/// Plan a handoff fold: same folded prefix as [`fold_prefix`], document written
/// over the whole transcript.
///
/// # Errors
/// Propagates summarizer failures.
pub async fn plan_handoff(
    messages: &[Message],
    cfg: &CompactionConfig,
    summarizer: &dyn Summarizer,
    options: SummarizeOptions,
) -> Result<Option<CompactionPlan>, CoreError> {
    if messages.len() <= cfg.keep_recent {
        return Ok(None);
    }
    let split = messages.len() - cfg.keep_recent;
    // `needs_compaction` guarantees `split >= 1`; stay panic-free regardless.
    let (Some(first), Some(last)) = (messages.first(), messages.get(split - 1)) else {
        return Ok(None);
    };
    let summary = summarizer
        .summarize(
            messages,
            SummarizeOptions {
                handoff: true,
                ..options
            },
        )
        .await?;
    Ok(Some(CompactionPlan {
        summary,
        from_message: first.id(),
        to_message: last.id(),
        folded_count: u32::try_from(split).unwrap_or(u32::MAX),
    }))
}

/// Compact `messages` when thresholds are exceeded; otherwise return them unchanged.
///
/// Request-local: the returned transcript is not persisted. Callers that must
/// record the compaction use [`plan_compaction`] and inject the summary
/// themselves.
///
/// # Errors
/// Propagates summarizer failures.
pub async fn compact_with(
    mut messages: Vec<Message>,
    cfg: &CompactionConfig,
    summarizer: &dyn Summarizer,
    options: SummarizeOptions,
) -> Result<Vec<Message>, CoreError> {
    let Some(plan) = plan_compaction(&messages, cfg, summarizer, options).await? else {
        return Ok(messages);
    };
    let split = messages.len() - cfg.keep_recent;
    let recent = messages.split_off(split);
    let older_count = plan.folded_count;
    let summary = plan.summary;
    let mut out = Vec::with_capacity(recent.len() + 1);
    out.push(Message::System {
        id: MessageId::new(),
        content: format!("Summary of {older_count} earlier messages:\n{summary}"),
    });
    out.extend(recent);
    Ok(out)
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
                let _ = writeln!(out, "[reasoning] {}", budgeted(text));
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
                            budgeted(&value_text(output))
                        );
                    }
                    hya_proto::ToolPartState::Error { input, message, .. } => {
                        let _ =
                            writeln!(out, "[tool {tool}] input: {}", budgeted(&input.to_string()));
                        let _ = writeln!(out, "[tool {tool}] failed: {}", budgeted(message));
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
        let request = CompletionRequest {
            model: options.model.unwrap_or_else(|| self.model.clone()),
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
        let mut stream = self
            .providers
            .stream(request, SessionId::new(), MessageId::new())
            .await?;
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            if let Event::TextDelta { delta, .. } = item? {
                text.push_str(&delta);
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_proto::PartId;

    struct Fake;
    #[async_trait]
    impl Summarizer for Fake {
        async fn summarize(
            &self,
            _messages: &[Message],
            _options: SummarizeOptions,
        ) -> Result<String, CoreError> {
            Ok("CONDENSED".to_string())
        }
    }

    fn user(text: &str) -> Message {
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn estimates_and_thresholds() {
        let msgs = vec![user(&"x".repeat(40))];
        assert_eq!(estimate_tokens(&msgs), 10);
        let cfg = CompactionConfig {
            token_threshold: 5,
            keep_recent: 0,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        assert!(needs_compaction(&msgs, &cfg));
    }

    #[test]
    fn estimate_tokens_counts_tool_output() {
        use hya_proto::{PartId, ToolCallId, ToolName, ToolPartState};
        let tool_body = "t".repeat(400);
        let msgs = vec![Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: vec![Part::Tool {
                id: PartId::new(),
                call_id: ToolCallId::new(),
                name: ToolName::new("find"),
                state: ToolPartState::Completed {
                    input: serde_json::json!({"pattern": "*"}),
                    output: serde_json::Value::String(tool_body.clone()),
                    time_ms: 1,
                },
            }],
            finish: None,
            tokens: None,
        }];
        // Text-only estimator would be ~0; tool body alone is 100 tokens.
        assert!(estimate_tokens(&msgs) >= tool_body.len() / 4);
        let cfg = CompactionConfig {
            token_threshold: 50,
            keep_recent: 0,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        assert!(needs_compaction(&msgs, &cfg));
    }

    #[tokio::test]
    async fn compacts_over_threshold_keeping_recent() {
        let msgs: Vec<Message> = (0..6).map(|_| user(&"y".repeat(40))).collect();
        let cfg = CompactionConfig {
            token_threshold: 10,
            keep_recent: 2,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        let out = compact_with(msgs, &cfg, &Fake, SummarizeOptions::default())
            .await
            .unwrap();
        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], Message::System { .. }));
        if let Message::System { content, .. } = &out[0] {
            assert!(content.contains("CONDENSED"));
            assert!(content.contains("4 earlier"));
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
        }
    }

    fn tool_output_of(message: &Message) -> Option<String> {
        let Message::Assistant { parts, .. } = message else {
            return None;
        };
        parts.iter().find_map(|p| match p {
            Part::Tool {
                state: hya_proto::ToolPartState::Completed { output, .. },
                ..
            } => output.as_str().map(ToString::to_string),
            _ => None,
        })
    }

    #[test]
    fn eviction_drops_stale_outputs_keeps_inputs_and_respects_keep_recent() {
        let mut msgs = vec![
            assistant_with_tool(&"OLD_OUTPUT_A".repeat(100)),
            assistant_with_tool(&"OLD_OUTPUT_B".repeat(100)),
            assistant_with_tool(&"RECENT_OUTPUT".repeat(100)),
        ];
        let before = estimate_tokens(&msgs);

        let evicted = evict_stale_tool_outputs(&mut msgs, 1, None);
        assert_eq!(evicted, 2, "only the two stale messages are evicted");
        assert!(
            estimate_tokens(&msgs) < before,
            "eviction must reduce the token count"
        );

        // Stale outputs replaced by the notice; the recent one is untouched.
        assert_eq!(
            tool_output_of(&msgs[0]).as_deref(),
            Some(EVICTED_OUTPUT_NOTICE)
        );
        assert_eq!(
            tool_output_of(&msgs[1]).as_deref(),
            Some(EVICTED_OUTPUT_NOTICE)
        );
        assert!(
            tool_output_of(&msgs[2]).is_some_and(|o| o.contains("RECENT_OUTPUT")),
            "the most recent tool output must survive"
        );

        // The call and its input survive, so the model still knows what it ran.
        let Message::Assistant { parts, .. } = &msgs[0] else {
            panic!("expected assistant");
        };
        let Part::Tool { name, state, .. } = &parts[0] else {
            panic!("expected tool part");
        };
        assert_eq!(name.as_str(), "find");
        let hya_proto::ToolPartState::Completed { input, .. } = state else {
            panic!("expected completed state");
        };
        assert_eq!(input["pattern"], "*.rs", "tool input must be preserved");
    }

    #[test]
    fn eviction_is_idempotent() {
        let mut msgs = vec![
            assistant_with_tool(&"OLD".repeat(200)),
            assistant_with_tool("recent"),
        ];
        assert_eq!(evict_stale_tool_outputs(&mut msgs, 1, None), 1);
        assert_eq!(
            evict_stale_tool_outputs(&mut msgs, 1, None),
            0,
            "a second pass has nothing left to evict"
        );
    }

    /// Records what it was handed, and hands back a deterministic handle.
    struct RecordingSink(std::sync::Mutex<Vec<(String, String)>>);

    impl RecordingSink {
        fn new() -> Self {
            Self(std::sync::Mutex::new(Vec::new()))
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.0.lock().expect("sink lock").clone()
        }
    }

    impl EvictionSink for RecordingSink {
        fn spill(&self, tool: &str, body: &str) -> Option<String> {
            let mut calls = self.0.lock().expect("sink lock");
            calls.push((tool.to_string(), body.to_string()));
            Some(format!("artifact://spill-{}", calls.len()))
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

    /// snapcompact is the no-LLM rung: a deterministic dense archive under the
    /// oh-my-pi budgets. A long tool output keeps its head and tail around an
    /// explicit omission marker, because errors and final state live at the
    /// ends of an output, not its middle.
    #[test]
    fn snapcompact_archive_keeps_head_and_tail_and_collapses_whitespace() {
        let body = format!(
            "{}{}{}",
            "H".repeat(2_000),
            "M".repeat(5_000),
            "T".repeat(2_000)
        );
        let mut msgs = vec![assistant_with_tool(&body)];
        msgs.push(user("a\n\n\n\nb   c"));
        let archive = snapcompact_archive(&msgs);
        assert!(
            archive.contains(&"H".repeat(100)),
            "head must survive: {archive}"
        );
        assert!(
            archive.contains(&"T".repeat(100)),
            "tail must survive: {archive}"
        );
        assert!(
            !archive.contains(&"M".repeat(50)),
            "the middle is omitted, not archived: {archive}"
        );
        assert!(archive.contains("chars omitted"), "{archive}");
        // Whitespace-collapsed: runs of blank lines and trailing space fold.
        assert!(
            archive.contains("a\n\nb c"),
            "whitespace is collapsed: {archive}"
        );
        assert_eq!(
            archive,
            snapcompact_archive(&msgs),
            "archive is deterministic"
        );
    }

    /// Tool-call arguments are capped per value and per call, matching omp's
    /// `SerializeOptions`, so one huge path glob cannot fill the archive.
    #[test]
    fn snapcompact_archive_budgets_tool_arguments() {
        let long_value = "v".repeat(1_000);
        let msgs = vec![Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: vec![Part::Tool {
                id: PartId::new(),
                call_id: hya_proto::ToolCallId::new(),
                name: hya_proto::ToolName::new("bash"),
                state: hya_proto::ToolPartState::Completed {
                    input: serde_json::json!({ "command": long_value }),
                    output: serde_json::Value::String("ok".to_string()),
                    time_ms: 1,
                },
            }],
            finish: None,
            tokens: None,
        }];
        let archive = snapcompact_archive(&msgs);
        assert!(
            !archive.contains(&"v".repeat(600)),
            "a single argument value is capped: {archive}"
        );
    }

    /// Records how many messages and what options it was handed.
    struct RecordingSummarizer(std::sync::Mutex<Vec<(usize, bool)>>);

    #[async_trait]
    impl Summarizer for RecordingSummarizer {
        async fn summarize(
            &self,
            messages: &[Message],
            options: SummarizeOptions,
        ) -> Result<String, CoreError> {
            self.0
                .lock()
                .expect("recorder lock")
                .push((messages.len(), options.handoff));
            Ok("HANDOFF".to_string())
        }
    }

    /// The handoff plan folds the same prefix a summary would, but the model
    /// writing the document reads the whole live transcript — the next agent
    /// must be told where the session *stands*, including recent turns.
    #[tokio::test]
    async fn plan_handoff_folds_the_prefix_but_shows_the_whole_transcript() {
        let msgs: Vec<Message> = (0..6)
            .map(|i| user(&format!("m{i} {}", "x".repeat(20))))
            .collect();
        let cfg = CompactionConfig {
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let recorder = RecordingSummarizer(std::sync::Mutex::new(Vec::new()));
        let plan = plan_handoff(&msgs, &cfg, &recorder, SummarizeOptions::default())
            .await
            .unwrap()
            .expect("over threshold must produce a plan");
        assert_eq!(plan.folded_count, 4);
        assert_eq!(plan.from_message, msgs[0].id());
        assert_eq!(plan.to_message, msgs[3].id());
        assert_eq!(plan.summary, "HANDOFF");
        let seen = recorder.0.lock().expect("recorder lock").clone();
        assert_eq!(
            seen,
            vec![(msgs.len(), true)],
            "the handoff call sees the whole transcript with handoff options"
        );
    }

    /// A transcript too small to fold hands back `None`, exactly like
    /// `fold_prefix`.
    #[tokio::test]
    async fn plan_handoff_declines_a_transcript_with_nothing_to_fold() {
        let msgs = vec![user("short")];
        let cfg = CompactionConfig {
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let recorder = RecordingSummarizer(std::sync::Mutex::new(Vec::new()));
        assert!(
            plan_handoff(&msgs, &cfg, &recorder, SummarizeOptions::default())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            recorder.0.lock().expect("recorder lock").is_empty(),
            "no foldable prefix means no model call"
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

    /// With a sink the body is preserved and the transcript says where it went,
    /// which is the difference between evicting output and losing it.
    #[test]
    fn eviction_with_a_sink_leaves_a_retrievable_handle() {
        let body = "OLD_OUTPUT_A".repeat(100);
        let mut msgs = vec![assistant_with_tool(&body), assistant_with_tool("recent")];
        let sink = RecordingSink::new();

        assert_eq!(evict_stale_tool_outputs(&mut msgs, 1, Some(&sink)), 1);

        let calls = sink.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0, "find",
            "the sink is told which tool produced it"
        );
        assert_eq!(
            calls[0].1, body,
            "the whole body is preserved, not a preview"
        );
        let notice = tool_output_of(&msgs[0]).expect("evicted output carries a notice");
        assert!(
            notice.contains("artifact://spill-1"),
            "the transcript must name the handle that retrieves the body: {notice}"
        );
    }

    /// A spilled notice is recognized as already-evicted, so a later pass does
    /// not spill the notice itself and count that as work.
    #[test]
    fn eviction_is_idempotent_over_spilled_notices() {
        let mut msgs = vec![
            assistant_with_tool(&"OLD".repeat(200)),
            assistant_with_tool("recent"),
        ];
        let sink = RecordingSink::new();

        assert_eq!(evict_stale_tool_outputs(&mut msgs, 1, Some(&sink)), 1);
        assert_eq!(
            evict_stale_tool_outputs(&mut msgs, 1, Some(&sink)),
            0,
            "a second pass has nothing left to evict"
        );
        assert_eq!(sink.calls().len(), 1, "the notice must not be re-spilled");
    }

    /// A sink that declines degrades to the lossy notice rather than failing the
    /// reduction that is trying to keep the turn inside its window.
    #[test]
    fn eviction_falls_back_to_the_lossy_notice_when_a_sink_declines() {
        struct Declines;
        impl EvictionSink for Declines {
            fn spill(&self, _tool: &str, _body: &str) -> Option<String> {
                None
            }
        }
        let mut msgs = vec![
            assistant_with_tool(&"OLD".repeat(200)),
            assistant_with_tool("recent"),
        ];

        assert_eq!(evict_stale_tool_outputs(&mut msgs, 1, Some(&Declines)), 1);
        assert_eq!(
            tool_output_of(&msgs[0]).as_deref(),
            Some(EVICTED_OUTPUT_NOTICE)
        );
    }

    #[test]
    fn resolved_threshold_scales_to_the_window_and_guards_bad_input() {
        let base = CompactionConfig {
            token_threshold: 100_000,
            keep_recent: 6,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        // No advertised window -> configured threshold stands (today's behaviour).
        assert_eq!(resolved_threshold(&base, None), 100_000);
        assert_eq!(resolved_threshold(&base, Some(0)), 100_000);
        // Advertised window -> scaled by the fraction.
        assert_eq!(resolved_threshold(&base, Some(200_000)), 150_000);
        assert_eq!(resolved_threshold(&base, Some(1_000_000)), 750_000);
        // A tiny window must not produce a compact-every-turn threshold.
        assert_eq!(
            resolved_threshold(&base, Some(100)),
            MIN_RESOLVED_THRESHOLD,
            "threshold is clamped to a usable floor"
        );
        // Nonsense fractions are not trusted.
        for bad in [0.0_f32, -1.0, 1.5, f32::NAN] {
            let cfg = CompactionConfig {
                context_fraction: bad,
                ..base
            };
            assert_eq!(
                resolved_threshold(&cfg, Some(200_000)),
                100_000,
                "fraction {bad} must fall back to the configured threshold"
            );
        }
    }

    /// The fraction alone can leave less headroom than the model needs for its
    /// response. `reserve_tokens` is the second bound, and the tighter of the
    /// two wins.
    #[test]
    fn resolved_threshold_reserves_room_for_the_response() {
        let cfg = CompactionConfig {
            token_threshold: 100_000,
            keep_recent: 6,
            context_fraction: 0.95,
            reserve_tokens: 16_384,
            summary_max_tokens: 4_096,
            method_order: CompactionRung::DEFAULT_ORDER,
        };
        // 0.95 * 200_000 = 190_000 leaves only 10_000 for the reply, so the
        // reserve is the binding constraint.
        assert_eq!(resolved_threshold(&cfg, Some(200_000)), 183_616);
        // With a tighter fraction the fraction binds instead.
        let loose = CompactionConfig {
            context_fraction: 0.5,
            ..cfg
        };
        assert_eq!(resolved_threshold(&loose, Some(200_000)), 100_000);
        // A reserve wider than the window still yields a usable floor.
        let huge = CompactionConfig {
            reserve_tokens: 500_000,
            ..cfg
        };
        assert_eq!(
            resolved_threshold(&huge, Some(200_000)),
            MIN_RESOLVED_THRESHOLD
        );
        // No advertised window: the flat threshold stands, reserve irrelevant.
        assert_eq!(resolved_threshold(&cfg, None), 100_000);
    }

    #[test]
    fn needs_compaction_at_honours_an_explicit_threshold() {
        let msgs: Vec<Message> = (0..8).map(|_| user(&"z".repeat(4000))).collect();
        let cfg = CompactionConfig {
            token_threshold: 100_000,
            keep_recent: 2,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        // 8 * 1000 = 8000 estimated tokens: under the flat 100k, over a 5k window.
        assert!(!needs_compaction(&msgs, &cfg));
        assert!(needs_compaction_at(&msgs, &cfg, 5_000));
    }

    fn assistant_with_usage(usage: Option<hya_proto::TokenUsage>) -> Message {
        Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: Vec::new(),
            finish: None,
            tokens: usage,
        }
    }

    #[test]
    fn measured_tokens_uses_reported_usage_plus_the_delta_since() {
        let usage = hya_proto::TokenUsage {
            input: 1000,
            output: 50,
            reasoning: 0,
            cache_read: 200,
            cache_write: 0,
        };
        let msgs = vec![
            user(&"a".repeat(4000)), // would estimate to 1000 on its own
            assistant_with_usage(Some(usage)),
            user(&"b".repeat(400)), // appended after: estimates to 100
        ];
        // 1000 input + 200 cache_read + 100 estimated delta.
        assert_eq!(measured_tokens(&msgs), Some(1300));
        assert_eq!(tokens_in_use(&msgs), 1300);
    }

    #[test]
    fn measured_tokens_ignores_empty_usage_and_falls_back_to_the_estimator() {
        // A route with usage_reporting: false never populates tokens; behaviour
        // must be byte-identical to the pre-change estimator path.
        let msgs = vec![user(&"a".repeat(4000)), assistant_with_usage(None)];
        assert_eq!(measured_tokens(&msgs), None);
        assert_eq!(tokens_in_use(&msgs), estimate_tokens(&msgs));

        let zeroed = vec![
            user(&"a".repeat(4000)),
            assistant_with_usage(Some(hya_proto::TokenUsage::default())),
        ];
        assert_eq!(
            measured_tokens(&zeroed),
            None,
            "all-zero usage is not a measurement"
        );
        assert_eq!(tokens_in_use(&zeroed), estimate_tokens(&zeroed));
    }

    #[test]
    fn measured_tokens_prefers_the_most_recent_report() {
        let old = hya_proto::TokenUsage {
            input: 100,
            ..Default::default()
        };
        let new = hya_proto::TokenUsage {
            input: 900,
            ..Default::default()
        };
        let msgs = vec![
            assistant_with_usage(Some(old)),
            user("x"),
            assistant_with_usage(Some(new)),
        ];
        assert_eq!(measured_tokens(&msgs), Some(900));
    }

    #[tokio::test]
    async fn plan_reports_the_exact_folded_range() {
        let msgs: Vec<Message> = (0..6).map(|_| user(&"y".repeat(40))).collect();
        let cfg = CompactionConfig {
            token_threshold: 10,
            keep_recent: 2,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        let plan = plan_compaction(&msgs, &cfg, &Fake, SummarizeOptions::default())
            .await
            .unwrap()
            .expect("over threshold must produce a plan");
        // Folds the prefix before the retained recent messages: 6 - 2 = 4.
        assert_eq!(plan.folded_count, 4);
        assert_eq!(plan.from_message, msgs[0].id());
        assert_eq!(plan.to_message, msgs[3].id());
        assert_eq!(plan.summary, "CONDENSED");
    }

    #[tokio::test]
    async fn plan_is_none_under_threshold() {
        let msgs = vec![user("short")];
        let cfg = CompactionConfig {
            token_threshold: 1000,
            keep_recent: 2,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        assert!(
            plan_compaction(&msgs, &cfg, &Fake, SummarizeOptions::default())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn no_compaction_under_threshold() {
        let msgs = vec![user("short")];
        let cfg = CompactionConfig {
            token_threshold: 1000,
            keep_recent: 2,
            context_fraction: 0.75,
            ..CompactionConfig::default()
        };
        let out = compact_with(msgs, &cfg, &Fake, SummarizeOptions::default())
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
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
            rendered.contains("more bytes]"),
            "truncation must be stated so the model knows it saw a prefix"
        );
    }

    #[test]
    fn previous_summary_reads_the_anchored_marker() {
        let msgs = vec![
            Message::System {
                id: MessageId::new(),
                content: format!(
                    "{}\nEarlier: shipped the parser.",
                    hya_provider::COMPACT_CONTEXT_MARKER
                ),
            },
            user("continue"),
        ];
        assert_eq!(
            previous_summary(&msgs).as_deref(),
            Some("Earlier: shipped the parser.")
        );
    }

    #[test]
    fn previous_summary_ignores_unmarked_and_provider_windows() {
        let plain = vec![Message::System {
            id: MessageId::new(),
            content: "Summary of earlier conversation:\nstuff".to_string(),
        }];
        assert_eq!(
            previous_summary(&plain),
            None,
            "an unmarked system message is not an anchored summary"
        );

        let provider_window = vec![Message::System {
            id: MessageId::new(),
            content: format!(
                "{}\n{}\n[]",
                hya_provider::COMPACT_CONTEXT_MARKER,
                hya_provider::RESPONSES_COMPACT_ITEMS_MARKER
            ),
        }];
        assert_eq!(
            previous_summary(&provider_window),
            None,
            "a provider-folded window is serialized response items, not prose"
        );
    }
}
