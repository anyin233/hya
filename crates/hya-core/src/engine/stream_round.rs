use std::collections::HashMap;

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

pub(super) struct RoundFailure {
    pub(super) error: CoreError,
    pub(super) saw_tool_call: bool,
    pub(super) saw_text: bool,
}

impl From<CoreError> for RoundFailure {
    fn from(error: CoreError) -> Self {
        Self {
            error,
            saw_tool_call: false,
            saw_text: false,
        }
    }
}

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
    ) -> Result<StreamRound, RoundFailure> {
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
                (Ok(_), Err(error)) => return Err(error.into()),
                (Err(_), Err(error)) => {
                    tracing::warn!(%session, "usage record for a failed round was not appended: {error:#}");
                }
                (_, Ok(())) => {}
            }
        }
        let (tool_calls, finish) = collected.map_err(|failure| RoundFailure {
            error: failure.error,
            saw_tool_call: failure.saw_tool_call,
            saw_text: failure.saw_text,
        })?;
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
    ) -> Result<(Vec<ToolCallReq>, FinishReason), RoundFailure> {
        let mut tool_calls: Vec<ToolCallReq> = Vec::new();
        let mut text_parts = TextPartAccumulator::default();
        let mut reasoning_parts: HashMap<PartId, String> = HashMap::new();
        let mut active_text_part = None;
        let mut finish = FinishReason::Stop;
        let mut saw_text = false;
        while let Some(item) = stream.next().await {
            if let Err(error) = self.validate_actor_claim(actor_claim).await {
                return Err(RoundFailure {
                    error,
                    saw_tool_call: !tool_calls.is_empty(),
                    saw_text,
                });
            }
            let event = match item {
                Ok(event) => event,
                Err(error) => {
                    if let Some(part) = active_text_part
                        && let Some(text) = text_parts.text(part)
                        && let Err(error) = self
                            .persist_text_part(actor_claim, session, message, part, text)
                            .await
                    {
                        return Err(RoundFailure {
                            error,
                            saw_tool_call: !tool_calls.is_empty(),
                            saw_text,
                        });
                    }
                    for (part, text) in reasoning_parts {
                        if let Err(error) = self
                            .persist_reasoning_part(actor_claim, session, message, part, text, None)
                            .await
                        {
                            return Err(RoundFailure {
                                error,
                                saw_tool_call: !tool_calls.is_empty(),
                                saw_text,
                            });
                        }
                    }
                    return Err(RoundFailure {
                        error: error.into(),
                        saw_tool_call: !tool_calls.is_empty(),
                        saw_text,
                    });
                }
            };
            if matches!(&event, Event::TextStart { .. } | Event::TextDelta { .. }) {
                saw_text = true;
            }
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
                if let Event::TextStart { part, .. } = &event {
                    active_text_part = Some(*part);
                }
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
                if let Some((part, text)) = completed {
                    self.persist_text_part(actor_claim, session, message, part, text)
                        .await?;
                    active_text_part = None;
                }
                continue;
            }
            match event {
                Event::ReasoningStart { part, .. } => {
                    reasoning_parts.insert(part, String::new());
                    self.emit_for_actor(actor_claim, session, event).await?;
                }
                Event::ReasoningDelta { part, delta, .. } => {
                    if let Some(text) = reasoning_parts.get_mut(&part) {
                        text.push_str(&delta);
                    }
                    self.publish_live(Event::ReasoningDelta {
                        session,
                        message,
                        part,
                        delta,
                    });
                }
                Event::ReasoningEnd {
                    part,
                    provider_data,
                    ..
                } => {
                    let text = reasoning_parts.remove(&part).unwrap_or_default();
                    self.persist_reasoning_part(
                        actor_claim,
                        session,
                        message,
                        part,
                        text,
                        provider_data,
                    )
                    .await?;
                }
                event => self.emit_for_actor(actor_claim, session, event).await?,
            }
        }
        // A stream that ends without a part's `ReasoningEnd` still keeps the
        // thinking it streamed: only its deltas were live-only.
        for (part, text) in reasoning_parts {
            self.persist_reasoning_part(actor_claim, session, message, part, text, None)
                .await?;
        }
        Ok((tool_calls, finish))
    }

    async fn persist_reasoning_part(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
        provider_data: Option<serde_json::Value>,
    ) -> Result<(), CoreError> {
        self.emit_for_actor(
            actor_claim,
            session,
            Event::ReasoningReplace {
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
            Event::ReasoningEnd {
                session,
                message,
                part,
                provider_data,
            },
        )
        .await
    }

    async fn persist_text_part(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Result<(), CoreError> {
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
        .await
    }
}

fn merge_tokens(target: &mut Option<TokenUsage>, update: Option<TokenUsage>) {
    if let Some(update) = update {
        target.get_or_insert_with(TokenUsage::default).merge(update);
    }
}
