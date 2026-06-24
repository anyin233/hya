use futures::StreamExt;
use yaca_proto::{Event, FinishReason, MessageId, PartId, SessionId, TokenUsage, ToolCallId};
use yaca_provider::EventStream;

use super::SessionEngine;
use super::text_complete::TextPartAccumulator;
use crate::error::CoreError;

pub(super) struct StreamRound {
    pub(super) tool_calls: Vec<ToolCallReq>,
    pub(super) finish: FinishReason,
    pub(super) tokens: Option<TokenUsage>,
}

pub(super) struct ToolCallReq {
    pub(super) part: PartId,
    pub(super) call: ToolCallId,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
}

impl SessionEngine {
    pub(super) async fn collect_stream_round(
        &self,
        session: SessionId,
        message: MessageId,
        mut stream: EventStream,
    ) -> Result<StreamRound, CoreError> {
        let mut tool_calls: Vec<ToolCallReq> = Vec::new();
        let mut text_parts = TextPartAccumulator::default();
        let mut finish = FinishReason::Stop;
        let mut tokens = None;
        while let Some(item) = stream.next().await {
            let event = item?;
            if let Event::ToolCallRequested {
                part,
                call,
                name,
                input,
                ..
            } = &event
            {
                tool_calls.push(ToolCallReq {
                    part: *part,
                    call: *call,
                    name: name.to_string(),
                    input: input.clone(),
                });
            }
            if let Event::MessageFinished {
                finish: f,
                tokens: provider_tokens,
                ..
            } = &event
            {
                finish = *f;
                merge_tokens(&mut tokens, *provider_tokens);
                continue;
            }
            if let Some((part, text)) = text_parts.apply(&event)
                && let Some(replacement) =
                    self.complete_text_part(session, message, part, text).await
            {
                text_parts.replace(part, replacement.clone());
                self.emit(
                    session,
                    Event::TextReplace {
                        session,
                        message,
                        part,
                        text: replacement,
                    },
                )
                .await?;
            }
            self.emit(session, event).await?;
        }
        Ok(StreamRound {
            tool_calls,
            finish,
            tokens,
        })
    }
}

fn merge_tokens(target: &mut Option<TokenUsage>, update: Option<TokenUsage>) {
    if let Some(update) = update {
        target.get_or_insert_with(TokenUsage::default).merge(update);
    }
}
