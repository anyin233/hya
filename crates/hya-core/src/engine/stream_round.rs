use futures::StreamExt;
use hya_proto::{Event, FinishReason, MessageId, PartId, SessionId, TokenUsage, ToolCallId};
use hya_provider::EventStream;

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
        let mut durable_text_parts: Vec<(PartId, String)> = Vec::new();
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
            if matches!(
                &event,
                Event::TextStart { .. } | Event::TextDelta { .. } | Event::TextEnd { .. }
            ) {
                let completed = if let Some((part, text)) = text_parts.apply(&event) {
                    let text = match self
                        .complete_text_part(session, message, part, text.clone())
                        .await
                    {
                        Some(replacement) => {
                            text_parts.replace(part, replacement.clone());
                            self.publish_live(Event::TextReplace {
                                session,
                                message,
                                part,
                                text: replacement.clone(),
                            });
                            replacement
                        }
                        None => text,
                    };
                    Some((part, text))
                } else {
                    None
                };
                self.publish_live(event);
                if let Some(part_text) = completed {
                    durable_text_parts.push(part_text);
                }
                continue;
            }
            self.emit(session, event).await?;
        }
        for (part, text) in durable_text_parts {
            self.emit(
                session,
                Event::TextStart {
                    session,
                    message,
                    part,
                },
            )
            .await?;
            self.emit(
                session,
                Event::TextReplace {
                    session,
                    message,
                    part,
                    text,
                },
            )
            .await?;
            self.emit(
                session,
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
            )
            .await?;
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
