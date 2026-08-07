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
    /// Approximate token threshold (chars/4) that triggers compaction.
    pub token_threshold: usize,
    /// Number of recent messages retained unsummarized.
    pub keep_recent: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            token_threshold: 100_000,
            keep_recent: 6,
        }
    }
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

/// Whether `messages` exceeds keep_recent and the token threshold.
#[must_use]
pub fn needs_compaction(messages: &[Message], cfg: &CompactionConfig) -> bool {
    messages.len() > cfg.keep_recent && estimate_tokens(messages) > cfg.token_threshold
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
    if !needs_compaction(messages, cfg) {
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
        };
        assert!(needs_compaction(&msgs, &cfg));
    }

    #[tokio::test]
    async fn compacts_over_threshold_keeping_recent() {
        let msgs: Vec<Message> = (0..6).map(|_| user(&"y".repeat(40))).collect();
        let cfg = CompactionConfig {
            token_threshold: 10,
            keep_recent: 2,
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

    #[tokio::test]
    async fn plan_reports_the_exact_folded_range() {
        let msgs: Vec<Message> = (0..6).map(|_| user(&"y".repeat(40))).collect();
        let cfg = CompactionConfig {
            token_threshold: 10,
            keep_recent: 2,
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
        };
        let out = compact_with(msgs, &cfg, &Fake, SummarizeOptions::default())
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
    }
}
