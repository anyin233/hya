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
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            token_threshold: 100_000,
            keep_recent: 6,
            context_fraction: 0.75,
        }
    }
}

/// Smallest threshold [`resolved_threshold`] will ever return.
///
/// A threshold near zero would compact on every single turn, which is worse than
/// not compacting at all.
pub const MIN_RESOLVED_THRESHOLD: usize = 1_000;

/// Token threshold for this turn: a share of the model's advertised context
/// window when one is known, else the configured flat threshold.
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
    scaled.max(MIN_RESOLVED_THRESHOLD)
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

fn parts_text(parts: &[Part]) -> String {
    let mut s = String::new();
    for p in parts {
        if let Part::Text { text, .. } = p {
            s.push_str(text);
        }
    }
    s
}

fn render_for_summary(messages: &[Message]) -> String {
    let mut s = String::new();
    for m in messages {
        let (role, text) = match m {
            Message::User { parts, .. } => ("user", parts_text(parts)),
            Message::Assistant { parts, .. } => ("assistant", parts_text(parts)),
            Message::System { content, .. } => ("system", content.clone()),
        };
        let _ = writeln!(s, "[{role}] {text}");
    }
    s
}

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
        let transcript = render_for_summary(messages);
        let prompt = format!(
            "Summarize the earlier conversation below into a compact briefing that preserves \
             decisions, facts, file paths, and open tasks. Be concise.\n\n{transcript}"
        );
        let request = CompletionRequest {
            model: options.model.unwrap_or_else(|| self.model.clone()),
            system: options.system,
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: prompt,
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: Some(1024),
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

    #[test]
    fn resolved_threshold_scales_to_the_window_and_guards_bad_input() {
        let base = CompactionConfig {
            token_threshold: 100_000,
            keep_recent: 6,
            context_fraction: 0.75,
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

    #[test]
    fn needs_compaction_at_honours_an_explicit_threshold() {
        let msgs: Vec<Message> = (0..8).map(|_| user(&"z".repeat(4000))).collect();
        let cfg = CompactionConfig {
            token_threshold: 100_000,
            keep_recent: 2,
            context_fraction: 0.75,
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
        };
        let out = compact_with(msgs, &cfg, &Fake, SummarizeOptions::default())
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }
}
