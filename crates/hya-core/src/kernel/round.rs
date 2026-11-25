//! One model round: drain a provider stream into durable events.

use std::pin::Pin;

use async_trait::async_trait;
use futures::StreamExt;
use hya_proto::{
    Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId, TokenUsage, ToolCallId,
    UsagePurpose,
};
use hya_provider::{EventStream, ProviderError};
use hya_tool::deadline::{RunDeadline, Stage};
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use super::Emit;
use crate::error::CoreError;

/// A tool call the model requested in a round.
pub struct ToolCall {
    /// Part that carries the call.
    pub part: PartId,
    /// Provider call id the result pairs with.
    pub call: ToolCallId,
    /// Requested tool name.
    pub name: String,
    /// Arguments as the model sent them.
    pub input: serde_json::Value,
}

impl ToolCall {
    /// Pair a successful outcome with the call that produced it.
    #[must_use]
    pub fn result_event(
        &self,
        session: SessionId,
        message: MessageId,
        output: serde_json::Value,
        time_ms: u64,
    ) -> Event {
        Event::ToolResult {
            session,
            message,
            part: self.part,
            call: self.call,
            output,
            time_ms,
        }
    }

    /// Pair a failed outcome with this call, including any structured error supplied by the host.
    #[must_use]
    pub fn error_event(
        &self,
        session: SessionId,
        message: MessageId,
        message_text: String,
        value: Option<serde_json::Value>,
    ) -> Event {
        Event::ToolError {
            session,
            message,
            part: self.part,
            call: self.call,
            message_text,
            value,
        }
    }
}

/// What a completed round asks for next.
pub struct Round {
    /// Calls to run, in stream order.
    pub tool_calls: Vec<ToolCall>,
    /// Why the stream ended.
    pub finish: FinishReason,
    /// Usage the provider reported for the round.
    pub tokens: Option<TokenUsage>,
    /// The stream showed only reasoning until a run-deadline stage, so it
    /// was stopped there (docs/kernel.md).
    pub deadline_cut: Option<DeadlineCut>,
}

/// A reasoning-only stream stopped at a run-deadline stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadlineCut {
    /// The newly reached stage, or None for a step capped before its next stage.
    pub stage: Option<Stage>,
    /// The reasoning the step streamed before the cut.
    pub reasoning: String,
}

/// A round that failed, with whether the model had already produced output.
pub struct Failure {
    /// Why the round failed.
    pub error: CoreError,
    /// The stream opened and the step was started in the log.
    pub step_started: bool,
    /// The model had requested a tool call.
    pub saw_tool_call: bool,
    /// The round had streamed nonempty assistant text.
    pub saw_text: bool,
}

/// Replaces a completed text part before it is persisted (the full mode's
/// `text.complete` hook).
#[async_trait]
pub trait CompleteText: Send + Sync {
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
    // Reasoning parts already ended, in stream order.
    reasoned: Vec<String>,
    // Ended reasoning parts that carry data a route replays.
    replayable: Vec<PartId>,
    finish: Option<FinishReason>,
    tokens: Option<TokenUsage>,
    saw_text: bool,
    saw_text_part: bool,
    saw_tool_start: bool,
    saw_reasoning: bool,
    deadline_cut: Option<DeadlineCut>,
}

/// Drain `stream`, then record the round's usage as `UsageRecorded`, also
/// when the stream failed after reporting it (that round was billed).
/// Recording on the failure path is best-effort and never masks the round's
/// own error.
#[allow(clippy::too_many_arguments)]
pub async fn collect(
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
            deadline_cut: drained.deadline_cut,
        }),
        Err(error) => Err(Failure {
            error,
            step_started: true,
            saw_tool_call: !drained.tool_calls.is_empty(),
            saw_text: drained.saw_text,
        }),
    }
}

struct ReasoningTimer<'a> {
    deadline: &'a RunDeadline,
    stage: Option<Stage>,
    sleep: Pin<Box<Sleep>>,
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
    // Armed once, by the round's first reasoning: waiting for the first
    // byte is never cut, and a stream that keeps reasoning still meets it.
    let mut checkpoint: Option<ReasoningTimer> = None;
    loop {
        let reasoning_only = !(drained.saw_text_part || drained.saw_tool_start);
        let item = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                drained.finish = Some(FinishReason::Cancelled);
                break;
            }
            // Ahead of the stream, so buffered chunks cannot hold off a
            // checkpoint that has passed.
            Some(stage) = async {
                let ReasoningTimer { deadline, stage: next, sleep } = checkpoint.as_mut()?;
                sleep.await;
                // A late wake can find the run past the stage it waited for.
                Some(next.map(|stage| deadline.stage().max(stage).min(Stage::Final)).or_else(|| {
                    deadline.next_checkpoint(session).and_then(|(delay, stage)| delay.is_zero().then_some(stage))
                }))
            }, if reasoning_only && checkpoint.is_some() => {
                let reasoning = drained
                    .reasoned
                    .iter()
                    .chain(drained.reasoning.0.iter().map(|(_, body)| body))
                    .filter(|body| !body.is_empty())
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                drained.deadline_cut = Some(DeadlineCut { stage, reasoning });
                drained.finish = Some(FinishReason::Stop);
                break;
            }
            item = stream.next() => match item {
                Some(item) => item,
                None => break,
            },
        };
        let event = match item {
            Ok(event) => event,
            // Every decoder's final frame is its last meaningful one: a body
            // that breaks after it loses nothing, and the round is complete.
            Err(ProviderError::StreamInterrupted(error)) if drained.finish.is_some() => {
                tracing::warn!(%session, "stream broke after its final frame: {error}");
                break;
            }
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
                // The step is over, even if the stream stays open.
                checkpoint = None;
                if let Some(update) = reported_tokens {
                    drained
                        .tokens
                        .get_or_insert_with(TokenUsage::default)
                        .merge(update);
                }
            }
            Event::TextStart { part, .. } => {
                drained.saw_text_part = true;
                drained.text.0.push((part, String::new()));
                emit.live(event);
            }
            Event::TextDelta {
                part, ref delta, ..
            } => {
                drained.saw_text_part = true;
                drained.saw_text |= !delta.is_empty();
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
                    drained.saw_text |= !replacement.is_empty();
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
            Event::ToolInputStart { .. } => {
                drained.saw_tool_start = true;
                emit.emit(session, vec![event]).await?;
            }
            Event::ToolInputDelta { .. } => emit.live(event),
            Event::ReasoningStart { part, .. } => {
                drained.reasoning.0.push((part, String::new()));
                emit.emit(session, vec![event]).await?;
            }
            Event::ReasoningDelta { part, delta, .. } => {
                if !(delta.is_empty()
                    || drained.finish.is_some()
                    || drained.saw_reasoning
                    || drained.saw_text_part
                    || drained.saw_tool_start)
                {
                    drained.saw_reasoning = true;
                    // A step that starts near or past the checkpoint gets the
                    // grace to finish thinking (docs/kernel.md).
                    checkpoint = hya_tool::deadline::current().and_then(|deadline| {
                        let (wait, stage) = deadline.next_reasoning_cut(session)?;
                        Some(ReasoningTimer {
                            deadline,
                            stage,
                            sleep: Box::pin(tokio::time::sleep(wait)),
                        })
                    });
                }
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
                drained.reasoned.push(body.clone());
                if provider_data.is_some() {
                    drained.replayable.push(part);
                }
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
                drained.saw_tool_start = true;
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
    persist_open(emit, session, message, drained).await?;
    if drained.deadline_cut.is_some() && !drained.replayable.is_empty() {
        // No answer or call of the cut step follows its reasoning, and a route
        // that replays the reasoning would pair it with a later step's: keep
        // the text, drop the replay data (docs/kernel.md).
        let strip = drained
            .replayable
            .iter()
            .map(|&part| Event::ReasoningEnd {
                session,
                message,
                part,
                provider_data: None,
            })
            .collect();
        emit.emit(session, strip).await?;
    }
    Ok(())
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

/// Events for one completed system message in the transcript.
pub fn system_message_events(session: SessionId, message: MessageId, text: String) -> Vec<Event> {
    let part = PartId::new();
    vec![
        Event::MessageStarted {
            session,
            message,
            role: Role::System,
            agent: None,
            model: None,
        },
        Event::TextStart {
            session,
            message,
            part,
        },
        Event::TextDelta {
            session,
            message,
            part,
            delta: text,
        },
        Event::TextEnd {
            session,
            message,
            part,
        },
        Event::MessageFinished {
            session,
            message,
            role: Role::System,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ]
}
