//! One model round: drain a provider stream into durable events.

use async_trait::async_trait;
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
pub(crate) struct ToolCall {
    pub(crate) part: PartId,
    pub(crate) call: ToolCallId,
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
}

/// What a completed round asks for next.
pub(crate) struct Round {
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) finish: FinishReason,
    pub(crate) tokens: Option<TokenUsage>,
}

/// A round that failed, with whether the model had already produced output.
pub(crate) struct Failure {
    pub(crate) error: CoreError,
    pub(crate) saw_tool_call: bool,
    pub(crate) saw_text: bool,
}

/// Replaces a completed text part before it is persisted (the full mode's
/// `text.complete` hook).
#[async_trait]
pub(crate) trait CompleteText: Send + Sync {
    /// The replacement for `text`, or `None` to keep it.
    async fn complete(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Option<String>;
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

/// What a stream produced so far.
#[derive(Default)]
struct Drained {
    tool_calls: Vec<ToolCall>,
    // Parts still streaming, in stream order.
    text: Open,
    reasoning: Open,
    finish: Option<FinishReason>,
    tokens: Option<TokenUsage>,
    saw_text: bool,
}

/// Drain `stream`, then record the round's usage as `UsageRecorded`, also
/// when the stream failed after reporting it (that round was billed).
/// Recording on the failure path is best-effort and never masks the round's
/// own error.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn collect(
    emit: &dyn Emit,
    complete: Option<&dyn CompleteText>,
    session: SessionId,
    message: MessageId,
    step: u32,
    model: &ModelRef,
    stream: EventStream,
    cancel: &CancellationToken,
) -> Result<Round, Failure> {
    let mut drained = Drained::default();
    let mut result = drain(
        emit,
        complete,
        session,
        message,
        stream,
        &mut drained,
        cancel,
    )
    .await;
    if let Some(usage) = drained.tokens.filter(|usage| !usage.is_zero()) {
        let record = emit
            .emit(
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
            .await;
        match (&result, record) {
            (Ok(()), Err(error)) => result = Err(error),
            (Err(_), Err(error)) => {
                tracing::warn!(%session, "usage record for a failed round was not appended: {error:#}");
            }
            (_, Ok(())) => {}
        }
    }
    match result {
        Ok(()) => Ok(Round {
            tool_calls: drained.tool_calls,
            finish: drained.finish.unwrap_or(FinishReason::Stop),
            tokens: drained.tokens,
        }),
        Err(error) => Err(Failure {
            error,
            saw_tool_call: !drained.tool_calls.is_empty(),
            saw_text: drained.saw_text,
        }),
    }
}

async fn drain(
    emit: &dyn Emit,
    complete: Option<&dyn CompleteText>,
    session: SessionId,
    message: MessageId,
    mut stream: EventStream,
    drained: &mut Drained,
    cancel: &CancellationToken,
) -> Result<(), CoreError> {
    loop {
        let item = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                drained.finish = Some(FinishReason::Cancelled);
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
                persist_open(emit, session, message, drained).await?;
                return Err(error.into());
            }
        };
        match event {
            Event::MessageFinished {
                finish: reported,
                tokens: reported_tokens,
                ..
            } => {
                drained.finish = Some(reported);
                if let Some(update) = reported_tokens {
                    drained
                        .tokens
                        .get_or_insert_with(TokenUsage::default)
                        .merge(update);
                }
            }
            Event::TextStart { part, .. } => {
                drained.saw_text = true;
                drained.text.0.push((part, String::new()));
                emit.live(event);
            }
            Event::TextDelta {
                part, ref delta, ..
            } => {
                drained.saw_text = true;
                drained.text.append(part, delta);
                emit.live(event);
            }
            Event::TextEnd { part, .. } => {
                let Some(mut body) = drained.text.take(part) else {
                    emit.live(event);
                    continue;
                };
                if let Some(complete) = complete
                    && let Some(replacement) = complete
                        .complete(session, message, part, body.clone())
                        .await
                {
                    emit.live(Event::TextReplace {
                        session,
                        message,
                        part,
                        text: replacement.clone(),
                    });
                    body = replacement;
                }
                emit.live(event);
                emit.emit(session, text_events(session, message, part, body))
                    .await?;
            }
            Event::ToolInputDelta { .. } => emit.live(event),
            Event::ReasoningStart { part, .. } => {
                drained.reasoning.0.push((part, String::new()));
                emit.emit(session, vec![event]).await?;
            }
            Event::ReasoningDelta { part, delta, .. } => {
                drained.reasoning.append(part, &delta);
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
                let body = drained.reasoning.take(part).unwrap_or_default();
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
                drained.tool_calls.push(ToolCall {
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
    persist_open(emit, session, message, drained).await
}

/// Persist text and reasoning parts whose end never arrived.
async fn persist_open(
    emit: &dyn Emit,
    session: SessionId,
    message: MessageId,
    drained: &mut Drained,
) -> Result<(), CoreError> {
    for (part, body) in std::mem::take(&mut drained.text.0) {
        emit.emit(session, text_events(session, message, part, body))
            .await?;
    }
    for (part, body) in std::mem::take(&mut drained.reasoning.0) {
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
