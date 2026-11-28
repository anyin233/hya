//! The three-primitive kernel (ADR-0033): model append, tool append, and
//! rewrite over the event log.
//!
//! Each round reads a [`View`] of the session projection to build its request.

/// File snapshots and lightweight workspace observations.
pub mod file_snapshot;
/// The kernel's minimal system prompt.
pub mod prompt;
/// Projection-to-request conversion.
pub mod request;
mod rewrite;
pub mod round;
/// The kernel's seven tools.
pub mod tools;
mod view;
mod workspace;
pub use workspace::WorkspaceWatches;

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hya_context::rewrite::SessionLog;
use hya_proto::context::{ReasoningReplayPolicy, ToolImageReplay, current_turn_start};
use hya_proto::event::ViewRef;
use hya_proto::{
    Event, FinishReason, Message, MessageId, ModelRef, PartProjection, Projection, SessionId,
    TokenUsage,
};
use hya_provider::{CompletionRequest, EventStream, ProviderRouter, invalid_tool_arguments};
use hya_store::SessionStore;
use hya_tool::ToolError;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::CoreError;
use rewrite::Initiator;
pub use rewrite::{before_round, rewrite_now};
pub use round::{CompleteText, DeadlineCut, Failure, Round, ToolCall};
pub use view::View;

/// Agent name the kernel renders assistant messages with in requests.
pub const AGENT: &str = "kernel";

/// The message a bare run receives once before it ends (docs/kernel.md).
pub const COMPLETION_CHECK: &str = "[hya] Completion check, sent once before this run ends. Compare the result with the request itself, not with your memory of it.
1. Re-read the request. List what it states literally: paths and file names, exact strings and output formats, commands and flags, values and counts, and anything it says is unknown or must not be assumed.
2. For each item, look at the result as it is now (the file on disk, the output of the real command) and compare it with the request's wording. Where the wording is exact, print the actual text and read it against the request's text, character for character. A program or pattern you wrote to do the comparison does not count, and neither does a substring or a summary. Where the request says a value is unknown, try the result with a value other than the one you can see.
3. For any requirement where you computed or considered more than one value, design, or interpretation, check that the delivered result uses the candidate best supported by the task's own materials—not a convention or prior—and that you did not drop or revert a fix for a failure you demonstrated. Do not reopen a verified external action outside its documented workflow, and do not change a verified result on convention alone.
4. If something differs, fix it and check again. If everything already matches, change nothing.
Then give your final answer.";

/// Whether a logged message is the shared completion check.
pub fn is_completion_check(text: &str) -> bool {
    // The opening line identifies logged checks across changes to the instructions.
    text.lines().next() == COMPLETION_CHECK.lines().next()
}

/// The message a bare run receives after a round with neither answer text nor
/// a tool call (docs/kernel.md). `{n}` is the round's output-token count; that
/// sentence is left out when the round reported no usage.
pub const EMPTY_TURN: &str = "[hya] Your last turn ended with thinking only: it gave no answer and made no tool call, so nothing was done in it. It ended after {n} output tokens. One turn's output is limited, so think in shorter turns. Continue from what you worked out: make the next tool call now, or give your final answer. Write results you will need later to a file or the todo list instead of holding them in thought.";

/// Empty turns in a row that get [`EMPTY_TURN`]; the next one ends the run.
pub const EMPTY_TURN_NOTICES: usize = 2;

/// Whether a logged message is a thinking-only continuation.
pub fn is_empty_turn_notice(text: &str) -> bool {
    text.split(':').next() == EMPTY_TURN.split(':').next()
}

/// Format the shared continuation notice with the usage reported for the step.
pub fn empty_turn_notice(output_tokens: Option<u64>) -> String {
    match output_tokens {
        Some(n) => EMPTY_TURN.replace("{n}", &n.to_string()),
        None => EMPTY_TURN.replace(" It ended after {n} output tokens.", ""),
    }
}

/// Minimum prior-round reasoning tokens required before checking whether
/// the endpoint dropped replayed reasoning from the next prompt.
const MIN_DROPPED_REASONING_TOKENS: u64 = 1_000;

const REASONING_DROPPED_PREFIX: &str = "[hya] Reasoning replay notice:";

/// Sent once per turn in both modes if provider usage shows the endpoint
/// dropped a prior step's replayed reasoning (docs/kernel.md).
const REASONING_DROPPED_NOTICE: &str = "[hya] Reasoning replay notice: the model endpoint did not carry your previous step's private reasoning into the next request. Do not rely on private thinking being available in later steps: keep key derivations, findings, decisions, and next steps in brief visible text alongside your tool calls, in a workspace file, or in the todo list.";

fn is_reasoning_dropped_notice(text: &str) -> bool {
    text.starts_with(REASONING_DROPPED_PREFIX)
}

/// Keep the premise and latest conclusion of a reasoning-only step.
pub fn format_reasoning_excerpt(reasoning: &str) -> String {
    const HEAD_CHARS: usize = 1_200;
    const TAIL_CHARS: usize = 2_000;
    let count = reasoning.chars().count();
    if count <= HEAD_CHARS + TAIL_CHARS {
        return reasoning.to_owned();
    }
    let head: String = reasoning.chars().take(HEAD_CHARS).collect();
    let tail: String = reasoning.chars().skip(count - TAIL_CHARS).collect();
    format!(
        "{head}\n… [{} chars omitted] …\n{tail}",
        count - HEAD_CHARS - TAIL_CHARS
    )
}

/// Carry completed reasoning into the next request, including on routes that omit its replay.
pub fn empty_turn_events(
    session: SessionId,
    parts: &[PartProjection],
    tokens: Option<u64>,
) -> Vec<Event> {
    let thoughts: Vec<_> = parts
        .iter()
        .filter_map(|part| match part {
            PartProjection::Reasoning { text, .. } if !text.trim().is_empty() => Some(text.trim()),
            _ => None,
        })
        .collect();
    let mut notice = empty_turn_notice(tokens);
    if !thoughts.is_empty() {
        notice.push_str("\n\nWhere your previous thinking left off:\n");
        notice.push_str(&format_reasoning_excerpt(&thoughts.join("\n\n")));
    }
    round::system_message_events(session, MessageId::new(), notice)
}

fn deadline_cut_notice(cut: &DeadlineCut, clock_or_stage: Option<String>) -> String {
    let explanation = if cut.stage.is_some() {
        "the run reached a deadline stage while your last step was still thinking, before \
         any answer or tool call, so that step was stopped. Do not restart the deliberation: \
         act on what you have worked out, with the next tool call or your final answer."
    } else {
        "your thinking-only step reached its wall-clock cap and was stopped to keep the run moving. \
         Act on what you worked out with a concrete tool call now."
    };
    let mut notice = format!(
        "{} {explanation}",
        hya_proto::context::REASONING_DEADLINE_NOTICE_PREFIX
    );
    if let Some(line) = clock_or_stage {
        notice.push_str("\n\n");
        notice.push_str(&line);
    }
    if !cut.reasoning.trim().is_empty() {
        notice.push_str("\n\nWhere your thinking stopped:\n");
        notice.push_str(&format_reasoning_excerpt(cut.reasoning.trim()));
    }
    notice
}

/// What one round with tool calls sent and reported, kept for the next
/// round's dropped-reasoning check (docs/kernel.md).
struct PrevRound {
    /// Model that served the round.
    model: ModelRef,
    /// Compaction marker the request started from.
    marker: Option<MessageId>,
    /// Tool outputs the request sent as eviction notices.
    evicted_parts: usize,
    /// The request attached tool-result images, sent only once.
    had_tool_image: bool,
    /// The request ended with a notice sent only once.
    had_transient_notice: bool,
    /// Usage the round reported.
    usage: Option<TokenUsage>,
}

impl PrevRound {
    /// Both rounds' usage when `next` sent this round's history plus its
    /// output, `replayed` this round's reasoning in it, yet `next`'s prompt
    /// grew by less than half the reasoning this round reported: the endpoint
    /// left that reasoning out.
    fn dropped_reasoning(&self, next: &Self, replayed: bool) -> Option<(TokenUsage, TokenUsage)> {
        let (prev, curr) = (self.usage?, next.usage?);
        (replayed
            && self.model == next.model
            && self.marker == next.marker
            && self.evicted_parts == next.evicted_parts
            // A tool-result image or trailing notice in either request lets a
            // route legitimately change earlier history or reasoning.
            && !(self.had_tool_image || next.had_tool_image)
            && !(self.had_transient_notice || next.had_transient_notice)
            && !prev.reasoning_unknown
            && prev.prompt() > 0
            && prev.reasoning >= MIN_DROPPED_REASONING_TOKENS
            && curr.prompt() > 0
            && curr.prompt() < prev.prompt().saturating_add(prev.reasoning / 2))
        .then_some((prev, curr))
    }
}

/// Whether `messages` replay, by `policy`, a reasoning part from the last
/// step of the turn's assistant `message` (the step before this request).
fn replays_last_step_reasoning(
    messages: &[Message],
    message: MessageId,
    policy: ReasoningReplayPolicy,
) -> bool {
    let turn_start = current_turn_start(messages);
    messages.iter().enumerate().any(|(index, row)| match row {
        Message::Assistant {
            id,
            parts,
            last_step_start: Some(start),
            ..
        } if *id == message => parts
            .get(*start..)
            .unwrap_or_default()
            .iter()
            .any(|part| policy.replays(turn_start, index, part)),
        _ => false,
    })
}

/// Where kernel events go: durable appends and live-only publishes.
#[async_trait]
pub trait Emit: Send + Sync {
    /// Append `events` to the session log as one batch, then publish them.
    async fn emit(&self, session: SessionId, events: Vec<Event>) -> Result<(), CoreError>;
    /// Publish without persisting (text and reasoning deltas).
    fn live(&self, event: Event);
}

/// Executes one tool call and appends its `ToolResult` or `ToolError`.
#[async_trait]
pub trait ToolGate: Send {
    /// Run `call` and record its outcome; an `Err` ends the run.
    async fn call(&mut self, call: ToolCall) -> Result<(), CoreError>;
}

/// What a harness supplies around the kernel's rounds: the rewrite before
/// each round, each round's request and stream, whether a failed round is
/// retried, and when the run stops.
#[async_trait]
pub trait Harness: ToolGate {
    /// Rewrite the Context before the round `projection` would send, if
    /// needed; `true` when the log changed.
    async fn before_round(&mut self, projection: &Projection) -> Result<bool, CoreError>;
    /// The round's request for `view`.
    async fn request(&mut self, view: &View<'_>) -> Result<CompletionRequest, CoreError>;
    /// Open `request`'s stream and name the model serving it; `None` when a
    /// cancel stopped the opening.
    async fn open(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<Option<(EventStream, ModelRef)>, CoreError>;
    /// A round attempt failed; `true` retries it, advancing the step only if it started.
    async fn failed(&mut self, failure: &Failure) -> Result<bool, CoreError>;
    /// A round attempt completed.
    async fn collected(&mut self, round: &Round) -> Result<(), CoreError>;
    /// A round ended as `Stop` or `Length` without tool calls and the run is
    /// not cancelled; `true` runs another round instead of ending (the
    /// harness has appended what that round answers).
    async fn before_end(&mut self) -> Result<bool, CoreError>;
    /// Whether the run ends after this round's tool calls.
    fn done(&self) -> bool;
    /// Replay policy and image-attachment capability of the live route that
    /// served `model`, when the harness can refresh providers mid-turn;
    /// `None` uses [`Scope::provider`].
    fn served_route(&self, _model: &ModelRef) -> Option<(ReasoningReplayPolicy, bool)> {
        None
    }
    /// The offered tool through which the agent delivers its result.
    fn deadline_delivery_tool(&self) -> Result<Option<&str>, CoreError> {
        Ok(None)
    }
}

/// Waits before retrying a round that failed on a transient provider error
/// (docs/kernel.md).
pub const ROUND_RETRY_WAITS: [Duration; 8] = [
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(40),
    Duration::from_secs(60),
    Duration::from_secs(60),
    Duration::from_secs(60),
    Duration::from_secs(60),
];

/// The run a round belongs to. As a [`SessionLog`] it reads the store and
/// appends through the run's [`Emit`].
pub struct Scope<'a> {
    /// Runtime-owned workspace observers shared across turns.
    pub watches: &'a WorkspaceWatches,
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

#[async_trait]
impl SessionLog for Scope<'_> {
    async fn projection(&self, session: SessionId) -> Result<Arc<Projection>, ToolError> {
        self.store
            .read_projection_shared(session)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))
    }

    async fn append(&self, session: SessionId, events: Vec<Event>) -> Result<(), ToolError> {
        self.emit
            .emit(session, events)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))
    }
}

/// Execute calls in order, keeping `rewrite` and `todo` in the kernel.
/// A call whose argument text was not valid JSON fails here, before any tool,
/// hook or permission check sees it. The engine closes unrun calls at turn end.
async fn execute_calls(
    scope: &Scope<'_>,
    model: &ModelRef,
    gate: &mut dyn ToolGate,
    calls: Vec<ToolCall>,
    cancel: &CancellationToken,
) -> Result<(), CoreError> {
    for call in calls {
        if let Some(error) = invalid_tool_arguments(&call.input) {
            let outcome = Err(format!(
                "arguments are not valid JSON: {error}. Nothing was run; call the tool \
                 again with its arguments as one valid JSON object."
            ));
            scope
                .emit
                .emit(
                    scope.session,
                    vec![tool_event(scope.session, scope.message, &call, outcome, 0)],
                )
                .await?;
        } else if call.name == tools::REWRITE {
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
        } else if call.name == tools::TODO {
            model_todo(scope, call).await?;
        } else {
            gate.call(call).await?;
        }
        if cancel.is_cancelled() {
            break;
        }
    }
    Ok(())
}

/// The kernel loop. Each round: read the projection, let the harness rewrite
/// it, request and stream one model round, then run its tool calls. Ends when
/// a round has no tool calls and the harness does not continue it, the
/// harness is done after a round's calls, or `cancel` fires.
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
    let mut prev: Option<PrevRound> = None;
    let mut reasoning_notice_delivered = false;
    // The notice this run appended and no request has carried yet.
    let mut pending_notice: Option<MessageId> = None;
    // A cut whose notice follows the next round's rewrite.
    let mut pending_cut: Option<DeadlineCut> = None;
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
        if let Some(cut) = pending_cut.take() {
            // Minted after any compaction marker of that rewrite, which
            // would otherwise leave the notice out of the request.
            let delivery_tool = harness.deadline_delivery_tool()?;
            let stage_notice = hya_tool::deadline::current().map(|deadline| match cut.stage {
                Some(stage) => deadline.notice(stage, delivery_tool),
                None => deadline.clock_line(),
            });
            emit.emit(
                session,
                round::system_message_events(
                    session,
                    MessageId::new(),
                    deadline_cut_notice(&cut, stage_notice),
                ),
            )
            .await?;
            projection = store.read_projection_shared(session).await?;
        }
        let mut batch = scope
            .watches
            .changes(store, session, projection.clone(), Default::default(), true)
            .await;
        let changes = std::mem::take(&mut batch.files);
        if let Some(event) = file_snapshot::observation(session, true, changes) {
            emit.emit(session, vec![event]).await?;
            projection = store.read_projection_shared(session).await?;
        }
        batch.committed();
        let view = View {
            projection: &projection,
        };
        let req = harness.request(&view).await?;
        let view_ref = ViewRef {
            cut: view.cut(),
            request_sha256: request::request_sha256(&req)?,
        };
        let visible = request::compacted_messages(&projection);
        let marker = visible
            .first()
            .filter(|row| request::is_marker(row))
            .map(|row| row.id);
        let evicted_parts = visible
            .iter()
            .flat_map(request::sent_parts)
            .filter(|part| {
                matches!(
                    part,
                    PartProjection::Tool {
                        eviction_notice: Some(_),
                        ..
                    }
                )
            })
            .count();
        let had_transient_notice = request::ends_with_transient_notice(&req.messages);
        let carries_reasoning_notice =
            pending_notice.is_some_and(|id| req.messages.iter().any(|m| m.id() == id));
        drop(projection);
        let (round, served) = loop {
            let failure = match harness.open(&req).await {
                Ok(None) => return Ok(FinishReason::Cancelled),
                Err(error) => Failure {
                    error,
                    step_started: false,
                    saw_tool_call: false,
                    saw_text: false,
                },
                Ok(Some((stream, served))) => {
                    emit.emit(
                        session,
                        vec![Event::StepStarted {
                            session,
                            message,
                            step,
                            view: Some(view_ref.clone()),
                        }],
                    )
                    .await?;
                    match round::collect(
                        emit, complete, session, message, step, &served, stream, cancel,
                    )
                    .await
                    {
                        Ok(round) => break (round, served),
                        Err(failure) => failure,
                    }
                }
            };
            if failure.step_started {
                emit.emit(
                    session,
                    vec![step_finished(
                        session,
                        message,
                        step,
                        FinishReason::Error,
                        Some(step_error_summary(&failure.error)),
                    )],
                )
                .await?;
            }
            if !harness.failed(&failure).await? {
                return Err(failure.error);
            }
            if failure.step_started {
                step += 1;
            }
        };
        harness.collected(&round).await?;
        reasoning_notice_delivered |= carries_reasoning_notice;
        pending_notice = None;
        // The route that served the round decides what its request replayed
        // and attached (a fallback model can differ from `req.model`).
        let (policy, sends_tool_images) = harness.served_route(&served).unwrap_or_else(|| {
            (
                scope.provider.reasoning_replay_policy(&served),
                scope.provider.sends_tool_images(&served),
            )
        });
        let replayed = policy.expands_next_prompt()
            && replays_last_step_reasoning(&req.messages, message, policy);
        let this = PrevRound {
            had_tool_image: ToolImageReplay::new(&req.messages, sends_tool_images).has_attached(),
            model: served,
            marker,
            evicted_parts,
            had_transient_notice,
            usage: round.tokens,
        };
        let dropped = prev
            .as_ref()
            .and_then(|prev| prev.dropped_reasoning(&this, replayed));
        if let Some((prev_usage, curr_usage)) = dropped {
            tracing::warn!(
                name: "reasoning_replay_dropped",
                %session,
                step,
                prev_prompt = prev_usage.prompt(),
                curr_prompt = curr_usage.prompt(),
                prev_reasoning = prev_usage.reasoning,
                "model endpoint did not carry prior step's reasoning into the next prompt"
            );
        }
        let had_calls = !round.tool_calls.is_empty();
        let notify_dropped = dropped.is_some() && !reasoning_notice_delivered && had_calls;
        emit.emit(
            session,
            vec![step_finished(session, message, step, round.finish, None)],
        )
        .await?;
        if round.finish == FinishReason::Cancelled {
            return Ok(FinishReason::Cancelled);
        }
        if let Some(cut) = round.deadline_cut {
            // The cut's notice tells the stage, so the next round's own
            // stage notice or clock line would repeat it.
            if let Some(deadline) = hya_tool::deadline::current() {
                if let Some(stage) = cut.stage {
                    deadline.mark_notified(session, stage);
                } else {
                    // The pending notice carries the clock for this budget tenth.
                    let _ = deadline.take_clock(session);
                }
            }
            pending_cut = Some(cut);
            prev = None;
            step += 1;
            continue;
        }
        // Only a normal end of the model's turn asks the harness to go on,
        // and not once cancelled.
        if round.tool_calls.is_empty()
            && !(matches!(round.finish, FinishReason::Stop | FinishReason::Length)
                && !cancel.is_cancelled()
                && harness.before_end().await?)
        {
            return Ok(round.finish);
        }
        execute_calls(scope, &req.model, harness, round.tool_calls, cancel).await?;
        if cancel.is_cancelled() {
            return Ok(FinishReason::Cancelled);
        }
        if harness.done() {
            return Ok(FinishReason::Stop);
        }
        if notify_dropped {
            // Minted after the calls' parts: the next request carries it, and
            // the parts of the next round leave it out of later ones.
            let notice = MessageId::new();
            emit.emit(
                session,
                round::system_message_events(session, notice, REASONING_DROPPED_NOTICE.to_string()),
            )
            .await?;
            pending_notice = Some(notice);
        }
        // A round without calls continues only with a harness notice, so the
        // next prompt is not comparable with this one.
        prev = had_calls.then_some(this);
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
    let pressure = rewrite::recorded_pressure(
        &projection,
        scope
            .provider
            .capabilities(model)
            .map(|caps| caps.max_context),
    );
    let rewrite = rewrite::rewrite_events(
        &projection,
        scope.session,
        scope.message,
        Initiator::Model,
        pressure,
    )?;
    let removed = match rewrite {
        Some((events, removed)) => {
            scope.emit.emit(scope.session, events).await?;
            Some(removed)
        }
        None => None,
    };
    Ok(match removed {
        Some(removed) => Ok(json!({
            "removed_messages": removed,
            "note": "The summary in this call is your own text, not an observation.",
        })),
        None => Err("nothing to remove".to_string()),
    })
}

/// Record the todo result and any list change in the same transaction.
async fn model_todo(scope: &Scope<'_>, mut call: ToolCall) -> Result<(), CoreError> {
    let started = Instant::now();
    let session = scope.session;
    let args = tools::args::<tools::todo::Args>(std::mem::take(&mut call.input));
    let projection = scope.store.read_projection_shared(session).await?;
    let current = projection.session.todos.as_deref().unwrap_or_default();
    let mut events = Vec::with_capacity(2);
    let result =
        match args.and_then(|args| args.apply(current.to_vec(), projection.session.todo_last_id)) {
            Ok(todos) => {
                let output = Value::String(hya_proto::context::render_todos(&todos));
                if todos != current {
                    events.push(Event::TodosUpdated { session, todos });
                }
                Ok(output)
            }
            Err(error) => Err(error.to_string()),
        };
    let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    events.insert(
        0,
        tool_event(session, scope.message, &call, result, time_ms),
    );
    scope.emit.emit(session, events).await
}

fn tool_event(
    session: SessionId,
    message: MessageId,
    call: &ToolCall,
    outcome: Result<serde_json::Value, String>,
    time_ms: u64,
) -> Event {
    match outcome {
        Ok(output) => call.result_event(session, message, output, time_ms),
        Err(message_text) => call.error_event(session, message, message_text, None),
    }
}

const STEP_ERROR_MAX_CHARS: usize = 512;

fn step_error_summary(error: &CoreError) -> String {
    let raw = error.to_string();
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= STEP_ERROR_MAX_CHARS {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(STEP_ERROR_MAX_CHARS - 1).collect();
    format!("{truncated}…")
}

fn step_finished(
    session: SessionId,
    message: MessageId,
    step: u32,
    finish: FinishReason,
    error: Option<String>,
) -> Event {
    Event::StepFinished {
        session,
        message,
        step,
        finish,
        error,
    }
}

#[cfg(test)]
mod step_error_tests {
    use super::*;
    use hya_provider::ProviderError;

    #[test]
    fn step_error_summary_collapses_whitespace_and_bounds_length() {
        let short = CoreError::Provider(Arc::new(ProviderError::StreamInterrupted(
            "line 1\n  line\t2".into(),
        )));
        assert_eq!(step_error_summary(&short), "http: line 1 line 2");
        let long = CoreError::Provider(Arc::new(ProviderError::StreamInterrupted("x".repeat(700))));
        let summary = step_error_summary(&long);
        assert!(summary.ends_with('…'));
        assert_eq!(summary.chars().count(), STEP_ERROR_MAX_CHARS);

        // Exact boundary: 512 chars total ("http: " is 6 chars + 506 'a's) must not truncate.
        let exact = CoreError::Provider(Arc::new(ProviderError::StreamInterrupted(
            "a".repeat(STEP_ERROR_MAX_CHARS - 6),
        )));
        let exact_summary = step_error_summary(&exact);
        assert!(!exact_summary.ends_with('…'));
        assert_eq!(exact_summary.chars().count(), STEP_ERROR_MAX_CHARS);

        // Multi-byte UTF-8 truncates cleanly on character boundaries.
        let multibyte =
            CoreError::Provider(Arc::new(ProviderError::StreamInterrupted("中".repeat(600))));
        let mb_summary = step_error_summary(&multibyte);
        assert!(mb_summary.ends_with('…'));
        assert_eq!(mb_summary.chars().count(), STEP_ERROR_MAX_CHARS);
    }
}
