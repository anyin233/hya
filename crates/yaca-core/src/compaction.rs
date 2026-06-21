use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use yaca_proto::{Event, Message, MessageId, ModelRef, Part, PartId, SessionId};
use yaca_provider::{CompletionRequest, ProviderRouter};

use crate::error::CoreError;

#[derive(Clone, Copy, Debug)]
pub struct CompactionConfig {
    pub token_threshold: usize,
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
        Message::User { parts, .. } | Message::Assistant { parts, .. } => parts
            .iter()
            .map(|p| match p {
                Part::Text { text, .. } => text.len(),
                _ => 0,
            })
            .sum(),
        Message::System { content, .. } => content.len(),
    }
}

#[must_use]
pub fn estimate_tokens(messages: &[Message]) -> usize {
    let chars: usize = messages.iter().map(message_text_len).sum();
    chars / 4
}

#[must_use]
pub fn needs_compaction(messages: &[Message], cfg: &CompactionConfig) -> bool {
    messages.len() > cfg.keep_recent && estimate_tokens(messages) > cfg.token_threshold
}

#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, messages: &[Message]) -> Result<String, CoreError>;
}

pub async fn compact_with(
    mut messages: Vec<Message>,
    cfg: &CompactionConfig,
    summarizer: &dyn Summarizer,
) -> Result<Vec<Message>, CoreError> {
    if !needs_compaction(&messages, cfg) {
        return Ok(messages);
    }
    let split = messages.len() - cfg.keep_recent;
    let recent = messages.split_off(split);
    let older_count = messages.len();
    let summary = summarizer.summarize(&messages).await?;
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

pub struct ModelSummarizer {
    providers: Arc<ProviderRouter>,
    model: ModelRef,
}

impl ModelSummarizer {
    #[must_use]
    pub fn new(providers: Arc<ProviderRouter>, model: ModelRef) -> Self {
        Self { providers, model }
    }
}

#[async_trait]
impl Summarizer for ModelSummarizer {
    async fn summarize(&self, messages: &[Message]) -> Result<String, CoreError> {
        let transcript = render_for_summary(messages);
        let prompt = format!(
            "Summarize the earlier conversation below into a compact briefing that preserves \
             decisions, facts, file paths, and open tasks. Be concise.\n\n{transcript}"
        );
        let request = CompletionRequest {
            model: self.model.clone(),
            system: Some("You compress conversation history. No tools.".to_string()),
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
    use yaca_proto::PartId;

    struct Fake;
    #[async_trait]
    impl Summarizer for Fake {
        async fn summarize(&self, _messages: &[Message]) -> Result<String, CoreError> {
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

    #[tokio::test]
    async fn compacts_over_threshold_keeping_recent() {
        let msgs: Vec<Message> = (0..6).map(|_| user(&"y".repeat(40))).collect();
        let cfg = CompactionConfig {
            token_threshold: 10,
            keep_recent: 2,
        };
        let out = compact_with(msgs, &cfg, &Fake).await.unwrap();
        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], Message::System { .. }));
        if let Message::System { content, .. } = &out[0] {
            assert!(content.contains("CONDENSED"));
            assert!(content.contains("4 earlier"));
        }
    }

    #[tokio::test]
    async fn no_compaction_under_threshold() {
        let msgs = vec![user("short")];
        let cfg = CompactionConfig {
            token_threshold: 1000,
            keep_recent: 2,
        };
        let out = compact_with(msgs, &cfg, &Fake).await.unwrap();
        assert_eq!(out.len(), 1);
    }
}
