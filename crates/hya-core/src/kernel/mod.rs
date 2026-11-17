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
pub(crate) mod round;
/// The kernel's five tools.
pub mod tools;

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, FinishReason, MessageId, ModelRef, SessionId,
};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{Tool, ToolCtx};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::error::CoreError;
use rewrite::Initiator;
pub(crate) use rewrite::Rewriter;
pub(crate) use round::ToolCall;

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
pub(crate) trait ToolGate: Send {
    /// Run `call` and record its outcome; an `Err` ends the run.
    async fn call(&mut self, call: ToolCall) -> Result<(), CoreError>;
}

/// Bare-mode [`ToolGate`]: looks the tool up by name and executes it.
struct BareGate<'a> {
    emit: &'a dyn Emit,
    tools: &'a [Arc<dyn Tool>],
    ctx: &'a ToolCtx,
    cancel: &'a CancellationToken,
    session: SessionId,
    message: MessageId,
}

#[async_trait]
impl ToolGate for BareGate<'_> {
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
        self.emit
            .emit(
                self.session,
                vec![tool_event(
                    self.session,
                    self.message,
                    &call,
                    outcome,
                    time_ms,
                )],
            )
            .await
    }
}

/// The run a round of tool calls belongs to.
pub(crate) struct Scope<'a> {
    /// Session event log.
    pub(crate) store: &'a SessionStore,
    /// Event sink.
    pub(crate) emit: &'a dyn Emit,
    /// Model routes; the round's route sizes a rewrite's recorded budget.
    pub(crate) provider: &'a ProviderRouter,
    /// Model of the round.
    pub(crate) model: &'a ModelRef,
    /// Session of the run.
    pub(crate) session: SessionId,
    /// Assistant message of the run.
    pub(crate) message: MessageId,
}

/// Execute a round's calls in order: `rewrite` in the kernel, every other
/// call through `gate`. When `cancel` fires, stops after the current call and
/// returns the calls that did not run.
pub(crate) async fn execute_calls(
    scope: &Scope<'_>,
    gate: &mut dyn ToolGate,
    calls: Vec<ToolCall>,
    cancel: &CancellationToken,
) -> Result<Vec<ToolCall>, CoreError> {
    let mut calls = calls.into_iter();
    while let Some(call) = calls.next() {
        if call.name == tools::REWRITE {
            let started = Instant::now();
            let outcome = model_rewrite(scope, &call).await?;
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
    let schemas: Vec<_> = tools.iter().map(|tool| tool.schema()).collect();
    let agent = AgentName::new(AGENT);
    let attachments = request::AttachmentData::new();
    let scope = Scope {
        store,
        emit,
        provider,
        model,
        session,
        message,
    };
    let mut gate = BareGate {
        emit,
        tools,
        ctx,
        cancel,
        session,
        message,
    };
    let mut rewriter = rewrite::HarnessRewriter(&scope);
    let mut step = 0;
    loop {
        if cancel.is_cancelled() {
            return Ok(FinishReason::Cancelled);
        }
        let projection = store.read_projection_shared(session).await?;
        if rewriter.before_round(&projection).await? {
            continue;
        }
        let req = request::request(
            &projection,
            system,
            schemas.clone(),
            model,
            &agent,
            &attachments,
            None,
        );
        let stream = provider.stream(req, session, message).await?;
        emit.emit(
            session,
            vec![Event::StepStarted {
                session,
                message,
                step,
            }],
        )
        .await?;
        let round =
            match round::collect(emit, None, session, message, step, model, stream, cancel).await {
                Ok(round) => round,
                Err(round::Failure { error, .. }) => {
                    emit.emit(
                        session,
                        vec![step_finished(session, message, step, FinishReason::Error)],
                    )
                    .await?;
                    return Err(error);
                }
            };
        emit.emit(
            session,
            vec![step_finished(session, message, step, round.finish)],
        )
        .await?;
        if round.finish == FinishReason::Cancelled {
            cancel_calls(emit, session, message, &round.tool_calls).await?;
            return Ok(FinishReason::Cancelled);
        }
        if round.tool_calls.is_empty() {
            return Ok(round.finish);
        }
        let unrun = execute_calls(&scope, &mut gate, round.tool_calls, cancel).await?;
        if cancel.is_cancelled() {
            cancel_calls(emit, session, message, &unrun).await?;
            return Ok(FinishReason::Cancelled);
        }
        step += 1;
    }
}

/// Run the model's `rewrite` call: its result, or why it failed.
async fn model_rewrite(
    scope: &Scope<'_>,
    call: &ToolCall,
) -> Result<Result<serde_json::Value, String>, CoreError> {
    if let Err(error) = tools::rewrite::check(&call.input) {
        return Ok(Err(error.to_string()));
    }
    let projection = scope.store.read_projection_shared(scope.session).await?;
    let budget = rewrite::pressure(
        &projection,
        scope.provider.capabilities(scope.model).as_ref(),
    );
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

/// Fail every call in `calls` as cancelled, so none is left without a result.
async fn cancel_calls(
    emit: &dyn Emit,
    session: SessionId,
    message: MessageId,
    calls: &[ToolCall],
) -> Result<(), CoreError> {
    if calls.is_empty() {
        return Ok(());
    }
    let events = calls
        .iter()
        .map(|call| tool_event(session, message, call, Err("cancelled".to_string()), 0))
        .collect();
    emit.emit(session, events).await
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
