use futures::StreamExt;
use hya_proto::{
    Event, FinishReason, MessageId, ModelRef, PartId, SessionId, TokenUsage, ToolCallId,
    UsagePurpose,
};
use hya_provider::EventStream;
use hya_store::ActorClaim;

use super::SessionEngine;
use super::text_complete::TextPartAccumulator;
use crate::error::CoreError;

pub(super) struct StreamRound {
    pub(super) tool_calls: Vec<ToolCallReq>,
    pub(super) finish: FinishReason,
    pub(super) tokens: Option<TokenUsage>,
}

/// Which round of the message a stream serves, and the model serving it.
pub(super) struct RoundAttribution {
    pub(super) step: u32,
    pub(super) model: ModelRef,
}

pub(super) struct ToolCallReq {
    pub(super) part: PartId,
    pub(super) call: ToolCallId,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
}

impl SessionEngine {
    /// Drain one provider round into durable events, then record its usage.
    ///
    /// The round's usage is appended as `UsageRecorded` attributed to
    /// `attribution.model` whether or not the round completes: a stream that
    /// reported usage and then failed was still billed. Recording on the
    /// failure path is best-effort and never masks the round's own error.
    pub(super) async fn collect_stream_round(
        &self,
        session: SessionId,
        message: MessageId,
        stream: EventStream,
        actor_claim: Option<&ActorClaim>,
        attribution: RoundAttribution,
    ) -> Result<StreamRound, CoreError> {
        let mut tokens = None;
        let collected = self
            .drain_stream_round(session, message, stream, actor_claim, &mut tokens)
            .await;
        if let Some(usage) = tokens.filter(|usage: &TokenUsage| !usage.is_zero()) {
            let record = self
                .emit_for_actor(
                    actor_claim,
                    session,
                    Event::UsageRecorded {
                        session,
                        message: Some(message),
                        step: Some(attribution.step),
                        model: attribution.model,
                        purpose: UsagePurpose::Turn,
                        tokens: usage,
                    },
                )
                .await;
            match (&collected, record) {
                (Ok(_), Err(error)) => return Err(error),
                (Err(_), Err(error)) => {
                    tracing::warn!(%session, "usage record for a failed round was not appended: {error:#}");
                }
                (_, Ok(())) => {}
            }
        }
        let (tool_calls, finish) = collected?;
        Ok(StreamRound {
            tool_calls,
            finish,
            tokens,
        })
    }

    async fn drain_stream_round(
        &self,
        session: SessionId,
        message: MessageId,
        mut stream: EventStream,
        actor_claim: Option<&ActorClaim>,
        tokens: &mut Option<TokenUsage>,
    ) -> Result<(Vec<ToolCallReq>, FinishReason), CoreError> {
        let mut tool_calls: Vec<ToolCallReq> = Vec::new();
        let mut durable_text_parts: Vec<(PartId, String)> = Vec::new();
        let mut text_parts = TextPartAccumulator::default();
        let mut finish = FinishReason::Stop;
        while let Some(item) = stream.next().await {
            self.validate_actor_claim(actor_claim).await?;
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
                merge_tokens(tokens, *provider_tokens);
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
            self.emit_for_actor(actor_claim, session, event).await?;
        }
        for (part, text) in durable_text_parts {
            self.emit_for_actor(
                actor_claim,
                session,
                Event::TextStart {
                    session,
                    message,
                    part,
                },
            )
            .await?;
            self.emit_for_actor(
                actor_claim,
                session,
                Event::TextReplace {
                    session,
                    message,
                    part,
                    text,
                },
            )
            .await?;
            self.emit_for_actor(
                actor_claim,
                session,
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
            )
            .await?;
        }
        Ok((tool_calls, finish))
    }
}

fn merge_tokens(target: &mut Option<TokenUsage>, update: Option<TokenUsage>) {
    if let Some(update) = update {
        target.get_or_insert_with(TokenUsage::default).merge(update);
    }
}
