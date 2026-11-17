//! One model round: drain a provider stream into durable events.

use futures::StreamExt;
use hya_proto::{
    Event, FinishReason, MessageId, ModelRef, PartId, SessionId, TokenUsage, ToolCallId,
    UsagePurpose,
};
use hya_provider::EventStream;
use tokio_util::sync::CancellationToken;

use super::Emit;
use crate::error::CoreError;

/// A tool call the model requested in a round.
pub(super) struct ToolCall {
    pub(super) part: PartId,
    pub(super) call: ToolCallId,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
}

/// What a completed round asks for next.
pub(super) struct Round {
    pub(super) tool_calls: Vec<ToolCall>,
    pub(super) finish: FinishReason,
}

#[derive(Default)]
struct Open(Vec<(PartId, String)>);

impl Open {
    fn append(&mut self, part: PartId, delta: &str) {
        if let Some((_, body)) = self.0.iter_mut().find(|(id, _)| *id == part) {
            body.push_str(delta);
        }
    }

    fn take(&mut self, part: PartId) -> Option<String> {
        let at = self.0.iter().position(|(id, _)| *id == part)?;
        Some(self.0.remove(at).1)
    }
}

/// Drain `stream`, then record the round's usage as `UsageRecorded`, also
/// when the stream failed after reporting it (that round was billed).
pub(super) async fn collect(
    emit: &dyn Emit,
    session: SessionId,
    message: MessageId,
    step: u32,
    model: &ModelRef,
    stream: EventStream,
    cancel: &CancellationToken,
) -> Result<Round, CoreError> {
    let mut tokens = None;
    let round = drain(emit, session, message, stream, &mut tokens, cancel).await;
    if let Some(usage) = tokens.filter(|usage: &TokenUsage| !usage.is_zero()) {
        emit.emit(
            session,
            vec![Event::UsageRecorded {
                session,
                message: Some(message),
                step: Some(step),
                model: model.clone(),
                purpose: UsagePurpose::Turn,
                tokens: usage,
            }],
        )
        .await?;
    }
    round
}

async fn drain(
    emit: &dyn Emit,
    session: SessionId,
    message: MessageId,
    mut stream: EventStream,
    tokens: &mut Option<TokenUsage>,
    cancel: &CancellationToken,
) -> Result<Round, CoreError> {
    let mut tool_calls = Vec::new();
    // Parts still streaming, in stream order.
    let mut text = Open::default();
    let mut reasoning = Open::default();
    let mut finish = FinishReason::Stop;
    loop {
        let item = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                finish = FinishReason::Cancelled;
                break;
            }
            item = stream.next() => match item {
                Some(item) => item,
                None => break,
            },
        };
        let event = match item {
            Ok(event) => event,
            Err(error) => {
                // Keep what streamed before the failure.
                persist_open(emit, session, message, text, reasoning).await?;
                return Err(error.into());
            }
        };
        match event {
            Event::MessageFinished {
                finish: reported,
                tokens: reported_tokens,
                ..
            } => {
                finish = reported;
                if let Some(update) = reported_tokens {
                    tokens.get_or_insert_with(TokenUsage::default).merge(update);
                }
            }
            Event::TextStart { part, .. } => {
                text.0.push((part, String::new()));
                emit.live(event);
            }
            Event::TextDelta {
                part, ref delta, ..
            } => {
                text.append(part, delta);
                emit.live(event);
            }
            Event::TextEnd { part, .. } => {
                emit.live(event);
                if let Some(body) = text.take(part) {
                    emit.emit(session, text_events(session, message, part, body))
                        .await?;
                }
            }
            Event::ToolInputDelta { .. } => emit.live(event),
            Event::ReasoningStart { part, .. } => {
                reasoning.0.push((part, String::new()));
                emit.emit(session, vec![event]).await?;
            }
            Event::ReasoningDelta { part, delta, .. } => {
                reasoning.append(part, &delta);
                emit.live(Event::ReasoningDelta {
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
                let body = reasoning.take(part).unwrap_or_default();
                emit.emit(
                    session,
                    reasoning_events(session, message, part, body, provider_data),
                )
                .await?;
            }
            Event::ToolCallRequested {
                part,
                call,
                ref name,
                ref input,
                ..
            } => {
                tool_calls.push(ToolCall {
                    part,
                    call,
                    name: name.to_string(),
                    input: input.clone(),
                });
                emit.emit(session, vec![event]).await?;
            }
            event => emit.emit(session, vec![event]).await?,
        }
    }
    persist_open(emit, session, message, text, reasoning).await?;
    Ok(Round { tool_calls, finish })
}

/// Persist text and reasoning parts whose end never arrived.
async fn persist_open(
    emit: &dyn Emit,
    session: SessionId,
    message: MessageId,
    text: Open,
    reasoning: Open,
) -> Result<(), CoreError> {
    for (part, body) in text.0 {
        emit.emit(session, text_events(session, message, part, body))
            .await?;
    }
    for (part, body) in reasoning.0 {
        emit.emit(
            session,
            reasoning_events(session, message, part, body, None),
        )
        .await?;
    }
    Ok(())
}

fn text_events(session: SessionId, message: MessageId, part: PartId, text: String) -> Vec<Event> {
    vec![
        Event::TextStart {
            session,
            message,
            part,
        },
        Event::TextReplace {
            session,
            message,
            part,
            text,
        },
        Event::TextEnd {
            session,
            message,
            part,
        },
    ]
}

fn reasoning_events(
    session: SessionId,
    message: MessageId,
    part: PartId,
    text: String,
    provider_data: Option<serde_json::Value>,
) -> Vec<Event> {
    vec![
        Event::ReasoningReplace {
            session,
            message,
            part,
            text,
        },
        Event::ReasoningEnd {
            session,
            message,
            part,
            provider_data,
        },
    ]
}
