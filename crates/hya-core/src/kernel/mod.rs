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
/// The kernel's six tools.
pub mod tools;
mod view;
mod workspace;
pub use workspace::WorkspaceWatches;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_context::rewrite::{ModelWindow, RewriteCtx, SessionLog, Transcript};
use hya_context::tokens::TokenAccounting;
use hya_proto::context::{ReasoningReplayPolicy, ToolImageReplay, current_turn_start};
use hya_proto::event::ViewRef;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, FinishReason, Message, MessageId, ModelRef,
    PartProjection, Projection, Role, SessionId, TokenUsage, ToolSchema,
};
use hya_provider::{
    CompletionRequest, EventStream, ProviderRetryClass, ProviderRouter, invalid_tool_arguments,
};
use hya_store::SessionStore;
use hya_tool::{Tool, ToolCtx, ToolError};
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

/// What a harness supplies around the kernel's rounds: the rewrite before
/// each round, each round's request and stream, whether a failed round is
/// retried, and when the run stops. Bare mode's [`Bare`] is the kernel alone;
/// full mode adds rebinding, hooks, and model fallback.
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
    /// Whether the run's deadline notices take the text for an agent that
    /// delivers by `report`.
    fn deadline_subagent(&self) -> Result<bool, CoreError> {
        Ok(false)
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

/// Bare mode's [`Harness`]: the given prompt and tools on one model, the
/// kernel's own context reduction, retry of a round after a transient
/// provider error.
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
    /// Retries of the current round so far.
    retries: usize,
    /// Output tokens the last collected round reported.
    output_tokens: Option<u64>,
    /// Empty turns in a row so far.
    empty_turns: usize,
}

#[async_trait]
impl ToolGate for Bare<'_> {
    async fn call(&mut self, mut call: ToolCall) -> Result<(), CoreError> {
        let started = Instant::now();
        let tool = self.tools.iter().find(|tool| tool.name() == call.name);
        let capture = if tool.is_some() {
            file_snapshot::FileCapture::bare(&call.name, &call.input, &self.ctx.workdir).await
        } else {
            file_snapshot::FileCapture::default()
        };
        let outcome = match tool {
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
            store,
            emit,
            session,
            message,
            ..
        } = *self.scope;
        let (changed, stamps) = capture
            .finish(
                store,
                session,
                message,
                call.call,
                outcome.as_ref().ok(),
                (self.scope.watches, &self.ctx.workdir),
            )
            .await?;
        let mut events = vec![tool_event(session, message, &call, outcome, time_ms)];
        events.extend(changed);
        events.extend(file_snapshot::observation(session, false, stamps));
        emit.emit(session, events).await
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

/// Bare mode's transcript: the kernel agent's view of the projection.
#[async_trait]
impl Transcript for Bare<'_> {
    async fn messages(&self, projection: &Projection) -> Result<Vec<Message>, ToolError> {
        Ok(request::projection_to_messages(
            &self.agent,
            projection,
            self.model,
            &self.attachments,
        ))
    }
}

#[async_trait]
impl Harness for Bare<'_> {
    /// The kernel's reduction with no extensions on the round's model window;
    /// no side model calls or hooks.
    async fn before_round(&mut self, projection: &Projection) -> Result<bool, CoreError> {
        let scope = self.scope;
        let replay = scope.provider.reasoning_replay_policy(self.model);
        let window = scope
            .provider
            .capabilities(self.model)
            .map(|caps| ModelWindow {
                max_context: caps.max_context,
                usage_reporting: caps.usage_reporting,
                reasoning_replay: replay,
                sends_tool_images: scope.provider.sends_tool_images(self.model),
            });
        let overhead_tokens = TokenAccounting::default().estimate_request(
            &[],
            Some(self.system),
            &self.schemas,
            replay,
        );
        let cx = RewriteCtx {
            session: scope.session,
            message: scope.message,
            model: self.model,
            projection,
            window,
            overhead_tokens,
            pressure: None,
            workdir: &self.ctx.workdir,
            config: &Value::Null,
            log: scope,
            transcript: &*self,
            models: None,
            hooks: None,
        };
        let rewritten = before_round(cx, &[]).await?;
        if let Some(deadline) = hya_tool::deadline::current()
            && let Some(notice) = deadline
                .take_notice(scope.session)
                .map(|stage| deadline.notice(stage, false))
                .or_else(|| deadline.take_clock(scope.session))
        {
            scope
                .emit
                .emit(
                    scope.session,
                    round::system_message_events(scope.session, MessageId::new(), notice),
                )
                .await?;
            return Ok(true);
        }
        Ok(rewritten)
    }

    async fn request(&mut self, view: &View<'_>) -> Result<CompletionRequest, CoreError> {
        Ok(request::request(
            view,
            self.system,
            self.schemas.clone(),
            self.model,
            &self.agent,
            &self.attachments,
            self.scope.provider.reasoning_default(self.model),
        ))
    }

    async fn open(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<Option<(EventStream, ModelRef)>, CoreError> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Ok(None),
            stream = self.scope.provider.stream(
                request.clone(),
                self.scope.session,
                self.scope.message,
            ) => Ok(Some((stream?, self.model.clone()))),
        }
    }

    /// Retry a transient provider failure or stream interruption. Before the
    /// wait, the failed attempt's parts leave the conversation.
    async fn failed(&mut self, failure: &Failure) -> Result<bool, CoreError> {
        let CoreError::Provider(error) = &failure.error else {
            return Ok(false);
        };
        let Some(&wait) = ROUND_RETRY_WAITS.get(self.retries) else {
            return Ok(false);
        };
        let retryable = matches!(
            error.retry_class(),
            ProviderRetryClass::Transient | ProviderRetryClass::InterruptedBody
        );
        let deadline = hya_tool::deadline::current();
        if !retryable
            || self.cancel.is_cancelled()
            || deadline.is_some_and(|d| d.remaining().saturating_sub(d.final_window()) <= wait)
        {
            return Ok(false);
        }
        let Scope {
            store,
            emit,
            session,
            message,
            ..
        } = *self.scope;
        // None of these parts was acted on: `drive` runs a round's tool calls
        // only after the round collected without error.
        let discarded: Vec<_> = store
            .with_projection(session, |projection| {
                projection
                    .session
                    .messages
                    .iter()
                    .find(|row| row.id == message && failure.step_started)
                    .and_then(|row| row.parts.get(row.last_step_start?..))
                    .unwrap_or_default()
                    .iter()
                    .map(|part| Event::PartDeleted {
                        session,
                        message,
                        part: part.id(),
                    })
                    .collect()
            })
            .await?;
        if !discarded.is_empty() {
            emit.emit(session, discarded).await?;
        }
        tracing::warn!(
            name: "bare_round_retry",
            %session,
            retry = self.retries + 1,
            "bare round failed: {error}; waiting {wait:?} before retry"
        );
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Ok(false),
            () = tokio::time::sleep(wait) => {}
        }
        if self.cancel.is_cancelled()
            || deadline.is_some_and(|d| d.stage() >= hya_tool::deadline::Stage::Final)
        {
            return Ok(false);
        }
        self.retries += 1;
        Ok(true)
    }

    async fn collected(&mut self, round: &Round) -> Result<(), CoreError> {
        self.retries = 0;
        self.output_tokens = round.tokens.map(|tokens| tokens.output);
        if !round.tool_calls.is_empty() {
            self.empty_turns = 0;
        }
        Ok(())
    }

    /// A turn without answer text continues the run with [`EMPTY_TURN`], at
    /// most [`EMPTY_TURN_NOTICES`] times in a row. Otherwise the completion
    /// check, once per session after the kernel ran a known tool, while more
    /// than two final windows remain on any run deadline.
    async fn before_end(&mut self) -> Result<bool, CoreError> {
        let Scope {
            store,
            emit,
            session,
            message,
            ..
        } = *self.scope;
        let projection = store.read_projection_shared(session).await?;
        let messages = &projection.session.messages;
        let parts = messages
            .iter()
            .find(|row| row.id == message)
            .and_then(|row| row.parts.get(row.last_step_start.unwrap_or(0)..))
            .unwrap_or_default();
        let answered = parts.iter().any(
            |part| matches!(part, PartProjection::Text { text, .. } if !text.trim().is_empty()),
        );
        if !answered {
            // Cancelled meanwhile: a logged notice would never be sent.
            if self.empty_turns == EMPTY_TURN_NOTICES || self.cancel.is_cancelled() {
                return Ok(false);
            }
            self.empty_turns += 1;
            emit.emit(
                session,
                empty_turn_events(session, parts, self.output_tokens),
            )
            .await?;
            return Ok(true);
        }
        self.empty_turns = 0;
        if hya_tool::deadline::current()
            .is_some_and(|deadline| deadline.remaining() <= 2 * deadline.final_window())
        {
            return Ok(false);
        }
        // A call of an unknown tool only recorded an error; `execute_calls`
        // runs `rewrite` and `todo` itself.
        let ran_tool = messages
            .iter()
            .flat_map(|message| &message.parts)
            .any(|part| {
                matches!(part, PartProjection::Tool { name, .. }
                if [tools::REWRITE, tools::TODO].contains(&name.as_str())
                    || self.tools.iter().any(|tool| tool.name() == name.as_str()))
            });
        // The logged check counts as sent even after requests leave it out.
        let sent = messages.iter().any(|message| {
            message.role == Role::System
                && is_completion_check(&request::collect_text(&message.parts))
        });
        // Cancelled meanwhile: a logged check would never be sent.
        if !ran_tool || sent || self.cancel.is_cancelled() {
            return Ok(false);
        }
        emit.emit(
            session,
            round::system_message_events(session, MessageId::new(), COMPLETION_CHECK.to_string()),
        )
        .await?;
        Ok(true)
    }

    fn done(&self) -> bool {
        false
    }
}

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
/// hook or permission check sees it. When cancelled, return the calls that did
/// not run.
async fn execute_calls(
    scope: &Scope<'_>,
    model: &ModelRef,
    gate: &mut dyn ToolGate,
    calls: Vec<ToolCall>,
    cancel: &CancellationToken,
) -> Result<Vec<ToolCall>, CoreError> {
    let mut calls = calls.into_iter();
    while let Some(call) = calls.next() {
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
            return Ok(calls.collect());
        }
    }
    Ok(Vec::new())
}

/// Run model rounds and tool calls until a round ends without tool calls.
///
/// Returns that round's finish, or `Cancelled`. The caller finishes the
/// assistant message. A round that fails on a transient provider error can be
/// retried.
pub async fn run(run: Run<'_>) -> Result<FinishReason, CoreError> {
    run_with_watches(run, &WorkspaceWatches::default()).await
}

/// Run a bare turn with workspace watches retained across calls.
pub async fn run_with_watches(
    run: Run<'_>,
    watches: &WorkspaceWatches,
) -> Result<FinishReason, CoreError> {
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
        watches,
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
        retries: 0,
        output_tokens: None,
        empty_turns: 0,
    };
    drive(&scope, &mut bare, cancel).await
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
            let subagent = harness.deadline_subagent()?;
            let stage_notice = hya_tool::deadline::current().map(|deadline| match cut.stage {
                Some(stage) => deadline.notice(stage, subagent),
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
            harness.cancelled(round.tool_calls).await?;
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
        let unrun = execute_calls(scope, &req.model, harness, round.tool_calls, cancel).await?;
        if cancel.is_cancelled() {
            harness.cancelled(unrun).await?;
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
