//! The three-primitive kernel (ADR-0033): model append, tool append, and
//! rewrite over the event log.
//!
//! The live Context is the session projection; every request is
//! [`request::request`] of it, and every change to it is an event.

/// The kernel's minimal system prompt.
pub mod prompt;
/// Projection-to-request conversion.
pub mod request;
mod rewrite;
pub mod round;
/// The kernel's five tools.
pub mod tools;

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, FinishReason, MessageId, ModelRef, Projection, SessionId,
    ToolSchema,
};
use hya_provider::{CompletionRequest, EventStream, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{Tool, ToolCtx};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::error::CoreError;
use rewrite::Initiator;
pub use rewrite::Rewriter;
pub use round::{CompleteText, Failure, Round, ToolCall};

/// Agent name the kernel renders assistant messages with in requests.
pub const AGENT: &str = "kernel";

/// Where kernel events go: durable appends and live-only publishes.
#[async_trait]
pub trait Emit: Send + Sync {
    /// Append `events` to the session log as one batch, then publish them.
    async fn emit(&self, session: SessionId, events: Vec<Event>) -> Result<(), CoreError>;
    /// Publish without persisting (text and reasoning deltas).
    fn live(&self, event: Event);
}

/// Bare-mode [`Emit`]: appends to the store and hands each envelope to
/// `publish` (for example an event bus).
pub struct StoreEmit<'a> {
    /// Session event log.
    pub store: &'a SessionStore,
    /// Receives every appended envelope, and live-only envelopes with `seq` 0.
    pub publish: &'a (dyn Fn(Envelope) + Send + Sync),
}

#[async_trait]
impl Emit for StoreEmit<'_> {
    async fn emit(&self, session: SessionId, events: Vec<Event>) -> Result<(), CoreError> {
        for envelope in self.store.append_events(session, events).await? {
            (self.publish)(envelope);
        }
        Ok(())
    }

    fn live(&self, event: Event) {
        let ts_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        (self.publish)(Envelope {
            seq: EventSeq(0),
            ts_millis,
            event,
        });
    }
}

/// Inputs of one kernel run over an assistant message.
pub struct Run<'a> {
    /// Session event log; the projection is read from it every round.
    pub store: &'a SessionStore,
    /// Event sink.
    pub emit: &'a dyn Emit,
    /// Model routes.
    pub provider: &'a ProviderRouter,
    /// Model every round uses.
    pub model: &'a ModelRef,
    /// System prompt text.
    pub system: &'a str,
    /// Tools offered to the model, in schema order.
    pub tools: &'a [Arc<dyn Tool>],
    /// Tool execution context.
    pub ctx: &'a ToolCtx,
    /// Session to run in.
    pub session: SessionId,
    /// Assistant message the caller already started (`MessageStarted`).
    pub message: MessageId,
    /// Cancels the run between and during rounds and tool calls.
    pub cancel: &'a CancellationToken,
}

/// Executes one tool call and appends its `ToolResult` or `ToolError`.
#[async_trait]
pub trait ToolGate: Send {
    /// Run `call` and record its outcome; an `Err` ends the run.
    async fn call(&mut self, call: ToolCall) -> Result<(), CoreError>;
    /// Account for `calls`, which a cancel left unrun.
    async fn cancelled(&mut self, calls: Vec<ToolCall>) -> Result<(), CoreError>;
}

/// What a harness supplies around the kernel's rounds: each round's request
/// and stream, whether a failed round is retried, and when the run stops.
/// Bare mode's [`Bare`] is the kernel alone; full mode adds rebinding, hooks,
/// model fallback, and retry.
#[async_trait]
pub trait Harness: ToolGate + Rewriter {
    /// The round's request for `projection`.
    async fn request(&mut self, projection: &Projection) -> Result<CompletionRequest, CoreError>;
    /// Open `request`'s stream and name the model serving it; `None` when a
    /// cancel stopped the opening.
    async fn open(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<Option<(EventStream, ModelRef)>, CoreError>;
    /// A round attempt failed; `true` retries it under the next step.
    async fn failed(&mut self, failure: &Failure) -> Result<bool, CoreError>;
    /// A round attempt completed.
    async fn collected(&mut self, round: &Round) -> Result<(), CoreError>;
    /// Whether the run ends after this round's tool calls.
    fn done(&self) -> bool;
}

/// Bare mode's [`Harness`]: the given prompt and tools on one model, the
/// kernel's own rewrite trigger, no retry.
struct Bare<'a> {
    scope: &'a Scope<'a>,
    model: &'a ModelRef,
    system: &'a str,
    schemas: Vec<ToolSchema>,
    tools: &'a [Arc<dyn Tool>],
    ctx: &'a ToolCtx,
    cancel: &'a CancellationToken,
    agent: AgentName,
    attachments: request::AttachmentData,
}

#[async_trait]
impl ToolGate for Bare<'_> {
    async fn call(&mut self, mut call: ToolCall) -> Result<(), CoreError> {
        let started = Instant::now();
        let outcome = match self.tools.iter().find(|tool| tool.name() == call.name) {
            None => Err(format!("unknown tool: {}", call.name)),
            Some(tool) => tokio::select! {
                biased;
                () = self.cancel.cancelled() => Err("cancelled".to_string()),
                result = tool.execute(self.ctx, std::mem::take(&mut call.input)) => {
                    result.map_err(|error| error.to_string())
                }
            },
        };
        let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let Scope {
            emit,
            session,
            message,
            ..
        } = *self.scope;
        emit.emit(
            session,
            vec![tool_event(session, message, &call, outcome, time_ms)],
        )
        .await
    }

    /// Fail every unrun call as cancelled, so none is left without a result.
    async fn cancelled(&mut self, calls: Vec<ToolCall>) -> Result<(), CoreError> {
        if calls.is_empty() {
            return Ok(());
        }
        let Scope {
            emit,
            session,
            message,
            ..
        } = *self.scope;
        let events = calls
            .iter()
            .map(|call| tool_event(session, message, call, Err("cancelled".to_string()), 0))
            .collect();
        emit.emit(session, events).await
    }
}

/// Removes everything before the next step once the last round filled the
/// kernel's trigger share of the window.
#[async_trait]
impl Rewriter for Bare<'_> {
    async fn before_round(&mut self, projection: &Projection) -> Result<bool, CoreError> {
        let scope = self.scope;
        let budget =
            rewrite::pressure(projection, scope.provider.capabilities(self.model).as_ref());
        Ok(budget.is_some_and(|(used, threshold)| used > threshold)
            && rewrite::commit_rewrite(
                scope.emit,
                projection,
                scope.session,
                scope.message,
                Initiator::Harness,
                budget,
            )
            .await?
            .is_some())
    }
}

#[async_trait]
impl Harness for Bare<'_> {
    async fn request(&mut self, projection: &Projection) -> Result<CompletionRequest, CoreError> {
        Ok(request::request(
            projection,
            self.system,
            self.schemas.clone(),
            self.model,
            &self.agent,
            &self.attachments,
            None,
        ))
    }

    async fn open(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<Option<(EventStream, ModelRef)>, CoreError> {
        let stream = self
            .scope
            .provider
            .stream(request.clone(), self.scope.session, self.scope.message)
            .await?;
        Ok(Some((stream, self.model.clone())))
    }

    async fn failed(&mut self, _failure: &Failure) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn collected(&mut self, _round: &Round) -> Result<(), CoreError> {
        Ok(())
    }

    fn done(&self) -> bool {
        false
    }
}

/// The run a round belongs to.
pub struct Scope<'a> {
    /// Session event log.
    pub store: &'a SessionStore,
    /// Event sink.
    pub emit: &'a dyn Emit,
    /// Replaces each completed text part before it is persisted.
    pub complete: Option<&'a dyn CompleteText>,
    /// Model routes; a round's route sizes a rewrite's recorded budget.
    pub provider: &'a ProviderRouter,
    /// Session of the run.
    pub session: SessionId,
    /// Assistant message of the run.
    pub message: MessageId,
}

/// Execute a round's calls in order: `rewrite` in the kernel, every other
/// call through `gate`. When `cancel` fires, stops after the current call and
/// returns the calls that did not run.
async fn execute_calls(
    scope: &Scope<'_>,
    model: &ModelRef,
    gate: &mut dyn ToolGate,
    calls: Vec<ToolCall>,
    cancel: &CancellationToken,
) -> Result<Vec<ToolCall>, CoreError> {
    let mut calls = calls.into_iter();
    while let Some(call) = calls.next() {
        if call.name == tools::REWRITE {
            let started = Instant::now();
            let outcome = model_rewrite(scope, model, &call).await?;
            let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            scope
                .emit
                .emit(
                    scope.session,
                    vec![tool_event(
                        scope.session,
                        scope.message,
                        &call,
                        outcome,
                        time_ms,
                    )],
                )
                .await?;
        } else {
            gate.call(call).await?;
        }
        if cancel.is_cancelled() {
            return Ok(calls.collect());
        }
    }
    Ok(Vec::new())
}

/// Run model rounds and tool calls until a round ends without tool calls.
///
/// Returns that round's finish, or `Cancelled`. The caller finishes the
/// assistant message. No retries, fallback, concurrency, or step cap.
pub async fn run(run: Run<'_>) -> Result<FinishReason, CoreError> {
    let Run {
        store,
        emit,
        provider,
        model,
        system,
        tools,
        ctx,
        session,
        message,
        cancel,
    } = run;
    let scope = Scope {
        store,
        emit,
        complete: None,
        provider,
        session,
        message,
    };
    let mut bare = Bare {
        scope: &scope,
        model,
        system,
        schemas: tools.iter().map(|tool| tool.schema()).collect(),
        tools,
        ctx,
        cancel,
        agent: AgentName::new(AGENT),
        attachments: request::AttachmentData::new(),
    };
    drive(&scope, &mut bare, cancel).await
}

/// The kernel loop. Each round: read the projection, let the harness rewrite
/// it, request and stream one model round, then run its tool calls. Ends when
/// a round has no tool calls, the harness is done after a round's calls, or
/// `cancel` fires.
///
/// Returns the last round's finish (`Stop` when the harness is done), or
/// `Cancelled`. The caller finishes the assistant message.
pub async fn drive(
    scope: &Scope<'_>,
    harness: &mut dyn Harness,
    cancel: &CancellationToken,
) -> Result<FinishReason, CoreError> {
    let &Scope {
        store,
        emit,
        complete,
        session,
        message,
        ..
    } = scope;
    let mut step = 0;
    loop {
        if cancel.is_cancelled() {
            return Ok(FinishReason::Cancelled);
        }
        let mut projection = store.read_projection_shared(session).await?;
        // A rewrite is events in the log; the request is the re-read projection's.
        if harness.before_round(&projection).await? {
            if cancel.is_cancelled() {
                return Ok(FinishReason::Cancelled);
            }
            projection = store.read_projection_shared(session).await?;
        }
        let req = harness.request(&projection).await?;
        drop(projection);
        let round = loop {
            let Some((stream, served)) = harness.open(&req).await? else {
                return Ok(FinishReason::Cancelled);
            };
            emit.emit(
                session,
                vec![Event::StepStarted {
                    session,
                    message,
                    step,
                }],
            )
            .await?;
            match round::collect(
                emit, complete, session, message, step, &served, stream, cancel,
            )
            .await
            {
                Ok(round) => break round,
                Err(failure) => {
                    emit.emit(
                        session,
                        vec![step_finished(session, message, step, FinishReason::Error)],
                    )
                    .await?;
                    if !harness.failed(&failure).await? {
                        return Err(failure.error);
                    }
                    // A failed attempt keeps its step number in the log.
                    step += 1;
                }
            }
        };
        harness.collected(&round).await?;
        emit.emit(
            session,
            vec![step_finished(session, message, step, round.finish)],
        )
        .await?;
        if round.finish == FinishReason::Cancelled {
            harness.cancelled(round.tool_calls).await?;
            return Ok(FinishReason::Cancelled);
        }
        if round.tool_calls.is_empty() {
            return Ok(round.finish);
        }
        let unrun = execute_calls(scope, &req.model, harness, round.tool_calls, cancel).await?;
        if cancel.is_cancelled() {
            harness.cancelled(unrun).await?;
            return Ok(FinishReason::Cancelled);
        }
        if harness.done() {
            return Ok(FinishReason::Stop);
        }
        step += 1;
    }
}

/// Run the model's `rewrite` call: its result, or why it failed.
async fn model_rewrite(
    scope: &Scope<'_>,
    model: &ModelRef,
    call: &ToolCall,
) -> Result<Result<serde_json::Value, String>, CoreError> {
    if let Err(error) = tools::rewrite::check(&call.input) {
        return Ok(Err(error.to_string()));
    }
    let projection = scope.store.read_projection_shared(scope.session).await?;
    let budget = rewrite::pressure(&projection, scope.provider.capabilities(model).as_ref());
    let removed = rewrite::commit_rewrite(
        scope.emit,
        &projection,
        scope.session,
        scope.message,
        Initiator::Model,
        budget,
    )
    .await?;
    Ok(match removed {
        Some(removed) => Ok(json!({
            "removed_messages": removed,
            "note": "The summary in this call is your own text, not an observation.",
        })),
        None => Err("nothing to remove".to_string()),
    })
}

fn tool_event(
    session: SessionId,
    message: MessageId,
    call: &ToolCall,
    outcome: Result<serde_json::Value, String>,
    time_ms: u64,
) -> Event {
    match outcome {
        Ok(output) => Event::ToolResult {
            session,
            message,
            part: call.part,
            call: call.call,
            output,
            time_ms,
        },
        Err(message_text) => Event::ToolError {
            session,
            message,
            part: call.part,
            call: call.call,
            message_text,
            value: None,
        },
    }
}

fn step_finished(session: SessionId, message: MessageId, step: u32, finish: FinishReason) -> Event {
    Event::StepFinished {
        session,
        message,
        step,
        finish,
    }
}
