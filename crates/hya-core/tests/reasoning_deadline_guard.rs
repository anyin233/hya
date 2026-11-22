//! A model stream that has shown only reasoning is stopped at the run's next
//! deadline stage, and the kernel continues with a notice (docs/kernel.md).
//!
//! Own binary, and each case in its own child process: `deadline::install`
//! sets one clock per process. Tokio's clock is paused and only the case's
//! scripted stream moves it, to each event's time since the run started.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use hya_core::CoreError;
use hya_core::kernel::{self, DeadlineCut, Emit, StoreEmit, round};
use hya_proto::context::REASONING_DEADLINE_NOTICE_PREFIX;
use hya_proto::{
    AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, Role, SessionId,
    SessionKind, TokenUsage, ToolCallId, ToolName,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::deadline::{self, Stage};
use hya_tool::{PermissionPlane, PermissionRules, ToolCtx};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const CASE: &str = "HYA_REASONING_GUARD_CASE";
/// WrapUp starts 585 s into a 900 s budget, Final at 792 s.
const BUDGET: Duration = Duration::from_secs(900);
const WRAP_UP: Duration = Duration::from_secs(585);
const FINAL: Duration = Duration::from_secs(792);

#[test]
fn reasoning_deadline_guard() {
    std::thread::scope(|scope| {
        for case in [
            "cut",
            "text",
            "tool",
            "first-byte",
            "straddle",
            "late-start",
            "late-start-cut",
            "finished",
            "compaction",
            "replay-tool",
            "replay-text",
            "wrap-up",
            "final",
        ] {
            scope.spawn(move || {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "case_child", "--nocapture"])
                    .env(CASE, case)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
                    "{case}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            });
        }
    });
}

#[tokio::test(start_paused = true)]
async fn case_child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    deadline::install(BUDGET);
    // A blocking task holds off Tokio's auto-advance, which would otherwise
    // move the clock while SQLite works on its own thread.
    let (hold, held) = tokio::sync::oneshot::channel::<()>();
    tokio::task::spawn_blocking(move || held.blocking_recv());
    match case.as_str() {
        "cut" => reasoning_only_round_is_cut().await,
        "text" => text_disarms_the_guard().await,
        "tool" => tool_input_disarms_the_guard().await,
        "first-byte" => waiting_for_the_first_byte_is_not_cut().await,
        "straddle" => a_short_step_straddling_wrap_up_finishes().await,
        "late-start" => a_short_step_after_an_untold_stage_finishes().await,
        "late-start-cut" => a_long_step_after_an_untold_stage_is_cut().await,
        "finished" => a_finished_step_on_an_open_stream_is_not_cut().await,
        "compaction" => compaction_after_a_cut_keeps_its_notice().await,
        "replay-tool" => cut_reasoning_is_not_replayed_before_a_later_step(false).await,
        "replay-text" => cut_reasoning_is_not_replayed_before_a_later_step(true).await,
        "wrap-up" => wrap_up_cut_then_answer().await,
        "final" => final_cut_is_not_repeated().await,
        other => panic!("unknown case {other}"),
    }
    drop(hold);
}

/// `events`, each once the clock is moved to its time since the run started.
fn paced(events: impl Iterator<Item = (Duration, Event)> + Send + 'static) -> EventStream {
    let start = Instant::now() - deadline::current().unwrap().elapsed();
    stream::iter(events)
        .then(move |(at, event)| async move {
            tokio::time::advance((start + at).saturating_duration_since(Instant::now())).await;
            Ok(event)
        })
        .boxed()
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Builds one stream's events.
#[derive(Clone, Copy)]
struct Turn {
    session: SessionId,
    message: MessageId,
}

impl Turn {
    fn new() -> Self {
        Self {
            session: SessionId::new(),
            message: MessageId::new(),
        }
    }

    /// A reasoning part opened at `at` with `text`, closed when `close`.
    fn reasoning(
        self,
        at: Duration,
        part: PartId,
        text: &str,
        close: bool,
    ) -> Vec<(Duration, Event)> {
        let Self { session, message } = self;
        let mut events = vec![
            (
                at,
                Event::ReasoningStart {
                    session,
                    message,
                    part,
                    reason: None,
                },
            ),
            (
                at,
                Event::ReasoningDelta {
                    session,
                    message,
                    part,
                    delta: text.to_string(),
                },
            ),
        ];
        if close {
            events.push((
                at,
                Event::ReasoningEnd {
                    session,
                    message,
                    part,
                    provider_data: None,
                },
            ));
        }
        events
    }

    /// `delta` into `part` every 5 s after `from`, without end.
    fn think_on(
        self,
        from: Duration,
        part: PartId,
        delta: &'static str,
    ) -> impl Iterator<Item = (Duration, Event)> + Send + 'static {
        let Self { session, message } = self;
        (1..).map(move |n| {
            (
                from + secs(5 * n),
                Event::ReasoningDelta {
                    session,
                    message,
                    part,
                    delta: delta.to_string(),
                },
            )
        })
    }

    fn text(self, at: Duration, body: &str) -> Vec<(Duration, Event)> {
        let Self { session, message } = self;
        let part = PartId::new();
        vec![
            (
                at,
                Event::TextStart {
                    session,
                    message,
                    part,
                },
            ),
            (
                at,
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: body.to_string(),
                },
            ),
            (
                at,
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
            ),
        ]
    }

    /// A `read` call whose input starts at `start` and that is requested,
    /// ending the stream, at `requested`.
    fn tool_call(self, start: Duration, requested: Duration) -> Vec<(Duration, Event)> {
        let Self { session, message } = self;
        let (part, call, name) = (PartId::new(), ToolCallId::new(), ToolName::new("read"));
        vec![
            (
                start,
                Event::ToolInputStart {
                    session,
                    message,
                    part,
                    call,
                    name: name.clone(),
                },
            ),
            (
                requested,
                Event::ToolCallRequested {
                    session,
                    message,
                    part,
                    call,
                    name,
                    input: serde_json::json!({"path": "a.txt"}),
                    provider_data: None,
                },
            ),
            self.finished(requested, FinishReason::ToolCalls),
        ]
    }

    fn finished(self, at: Duration, finish: FinishReason) -> (Duration, Event) {
        let Self { session, message } = self;
        (
            at,
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish,
                tokens: None,
                cause: None,
            },
        )
    }
}

/// Round-level emits, kept in memory: no I/O, so no clock moves mid-round.
#[derive(Default)]
struct Log(Mutex<Vec<Event>>);

#[async_trait]
impl Emit for Log {
    async fn emit(&self, _session: SessionId, events: Vec<Event>) -> Result<(), CoreError> {
        self.0.lock().unwrap().extend(events);
        Ok(())
    }

    fn live(&self, _event: Event) {}
}

async fn collect(
    turn: Turn,
    events: impl Iterator<Item = (Duration, Event)> + Send + 'static,
) -> (round::Round, Vec<Event>) {
    let log = Log::default();
    let round = round::collect(
        &log,
        None,
        turn.session,
        turn.message,
        0,
        &ModelRef::new("paced/model"),
        paced(events),
        &CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|failure| panic!("round failed: {}", failure.error));
    (round, log.0.into_inner().unwrap())
}

/// A stream that keeps reasoning is cut at WrapUp with all its reasoning,
/// closed blocks and the open one, while its deltas still arrive.
async fn reasoning_only_round_is_cut() {
    let turn = Turn::new();
    let (closed, open) = (PartId::new(), PartId::new());
    let mut events = turn.reasoning(secs(0), closed, "plan the parser", true);
    events.extend(turn.reasoning(secs(0), open, "", false));
    let (round, log) = collect(
        turn,
        events.into_iter().chain(turn.think_on(secs(0), open, "x")),
    )
    .await;
    let elapsed = deadline::current().unwrap().elapsed();
    assert!(
        (WRAP_UP..=WRAP_UP + secs(5)).contains(&elapsed),
        "{elapsed:?}"
    );
    assert!(round.tool_calls.is_empty());
    assert_eq!(round.finish, FinishReason::Stop);
    let Some(DeadlineCut { stage, reasoning }) = round.deadline_cut else {
        panic!("reasoning-only stream crossed WrapUp uncut");
    };
    assert_eq!(stage, Stage::WrapUp);
    let thought = reasoning
        .strip_prefix("plan the parser\n\n")
        .unwrap_or_else(|| panic!("{reasoning}"));
    // One delta per 5 s, up to 585 s.
    assert!(
        (116..=117).contains(&thought.len()) && thought.chars().all(|c| c == 'x'),
        "{thought}"
    );
    // The open part is persisted with what streamed before the cut.
    assert!(log.iter().any(|event| matches!(
        event,
        Event::ReasoningReplace { part, text, .. } if *part == open && text == thought
    )));
}

/// Text before WrapUp disarms the guard: the round runs past it whole.
async fn text_disarms_the_guard() {
    let turn = Turn::new();
    let mut events = turn.reasoning(secs(0), PartId::new(), "draft", true);
    let part = PartId::new();
    let Turn { session, message } = turn;
    events.push((
        secs(10),
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    events.extend((2..140).map(|n| {
        (
            secs(5 * n),
            Event::TextDelta {
                session,
                message,
                part,
                delta: "word ".to_string(),
            },
        )
    }));
    events.push((
        secs(700),
        Event::TextEnd {
            session,
            message,
            part,
        },
    ));
    events.push(turn.finished(secs(700), FinishReason::Stop));
    let (round, _) = collect(turn, events.into_iter()).await;
    assert!(deadline::current().unwrap().elapsed() >= secs(700));
    assert_eq!(round.deadline_cut, None);
    assert_eq!(round.finish, FinishReason::Stop);
}

/// A tool call's input starting before WrapUp disarms the guard, though the
/// call is requested only after it.
async fn tool_input_disarms_the_guard() {
    let turn = Turn::new();
    let mut events = turn.reasoning(secs(0), PartId::new(), "need a file", true);
    events.extend(turn.tool_call(secs(10), secs(700)));
    let (round, _) = collect(turn, events.into_iter()).await;
    assert!(deadline::current().unwrap().elapsed() >= secs(700));
    assert_eq!(round.deadline_cut, None);
    assert_eq!(round.tool_calls.len(), 1);
}

/// Nothing streams until after WrapUp (a long wait for the first byte),
/// then the answer: no reasoning, no cut.
async fn waiting_for_the_first_byte_is_not_cut() {
    let turn = Turn::new();
    let mut events = turn.text(secs(600), "answer");
    events.push(turn.finished(secs(600), FinishReason::Stop));
    let (round, _) = collect(turn, events.into_iter()).await;
    assert!(deadline::current().unwrap().elapsed() >= secs(600));
    assert_eq!(round.deadline_cut, None);
}

/// A step that starts reasoning 5 s before WrapUp gets the reasoning grace
/// (27 s here): 15 s of thought and then a tool call is not cut.
async fn a_short_step_straddling_wrap_up_finishes() {
    short_step_finishes(WRAP_UP - secs(5)).await;
}

/// The same for a step that starts after WrapUp the session was not told
/// of (a full-mode round whose notice would come with its tool result).
async fn a_short_step_after_an_untold_stage_finishes() {
    short_step_finishes(secs(600)).await;
}

async fn short_step_finishes(from: Duration) {
    let turn = Turn::new();
    let part = PartId::new();
    let mut events = turn.reasoning(from, part, "look up the format", false);
    events.extend(turn.think_on(from, part, " then").take(3));
    events.extend(turn.tool_call(from + secs(15), from + secs(20)));
    let (round, _) = collect(turn, events.into_iter()).await;
    assert_eq!(round.deadline_cut, None);
    assert_eq!(round.tool_calls.len(), 1);
}

/// A step that starts after an untold WrapUp and keeps reasoning past the
/// grace (600 s + 27 s) is cut there, at WrapUp.
async fn a_long_step_after_an_untold_stage_is_cut() {
    let turn = Turn::new();
    let part = PartId::new();
    let events = turn
        .reasoning(secs(600), part, "compare the options", false)
        .into_iter()
        .chain(turn.think_on(secs(600), part, " again"));
    let (round, _) = collect(turn, events).await;
    let elapsed = deadline::current().unwrap().elapsed();
    assert!((secs(627)..=secs(630)).contains(&elapsed), "{elapsed:?}");
    let Some(DeadlineCut { stage, reasoning }) = round.deadline_cut else {
        panic!("reasoning past the grace was not cut");
    };
    assert_eq!(stage, Stage::WrapUp);
    assert_eq!(
        reasoning,
        format!("compare the options{}", " again".repeat(5))
    );
}

/// A step that reported its end is not cut, though the stream stays open
/// past WrapUp after it.
async fn a_finished_step_on_an_open_stream_is_not_cut() {
    let turn = Turn::new();
    let part = PartId::new();
    let Turn { session, message } = turn;
    let mut events = turn.reasoning(secs(0), part, "out of room", false);
    events.push(turn.finished(secs(100), FinishReason::Length));
    // A frame that carries nothing, long after the end.
    events.push((
        secs(700),
        Event::ReasoningDelta {
            session,
            message,
            part,
            delta: String::new(),
        },
    ));
    let (round, _) = collect(turn, events.into_iter()).await;
    assert!(deadline::current().unwrap().elapsed() >= secs(700));
    assert_eq!(round.finish, FinishReason::Length);
    assert_eq!(round.deadline_cut, None);
}

type Script = Box<dyn Fn(Turn) -> EventStream + Send + Sync>;

/// One scripted stream per request, in order; requests are kept.
struct Paced {
    scripts: Mutex<std::vec::IntoIter<Script>>,
    requests: Mutex<Vec<CompletionRequest>>,
    max_context: u32,
}

#[async_trait]
impl Provider for Paced {
    fn id(&self) -> &str {
        "paced"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            max_context: self.max_context,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .next()
            .expect("a scripted round");
        Ok(script(Turn { session, message }))
    }
}

struct Run {
    provider: Arc<Paced>,
    log: Vec<Event>,
    finish: FinishReason,
    /// What a request built after the turn would send.
    messages: Vec<Message>,
}

/// One bare turn of `session` over `scripts`, on a model with `max_context`.
async fn bare_turn(session: SessionId, max_context: u32, scripts: Vec<Script>) -> Run {
    let store = SessionStore::connect_memory().await.unwrap();
    let dir = std::env::temp_dir();
    let model = ModelRef::new("paced/model");
    let (user, part, message) = (MessageId::new(), PartId::new(), MessageId::new());
    store
        .append_events(
            session,
            vec![
                Event::SessionCreated {
                    session,
                    parent: None,
                    agent: AgentName::new(kernel::AGENT),
                    model: model.clone(),
                    workdir: dir.display().to_string(),
                    project: None,
                    kind: SessionKind::Project,
                },
                Event::MessageStarted {
                    session,
                    message: user,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session,
                    message: user,
                    part,
                },
                Event::TextDelta {
                    session,
                    message: user,
                    part,
                    delta: "write the report".to_string(),
                },
                Event::TextEnd {
                    session,
                    message: user,
                    part,
                },
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            ],
        )
        .await
        .unwrap();
    let provider = Arc::new(Paced {
        scripts: Mutex::new(scripts.into_iter()),
        requests: Mutex::default(),
        max_context,
    });
    let router = ProviderRouter::new().with(provider.clone());
    let cancel = CancellationToken::new();
    let ctx = tool_ctx(&dir, &cancel);
    let finish = kernel::run(kernel::Run {
        store: &store,
        emit: &StoreEmit {
            store: &store,
            publish: &|_| {},
        },
        provider: &router,
        model: &model,
        system: "system",
        tools: &[],
        ctx: &ctx,
        session,
        message,
        cancel: &cancel,
    })
    .await
    .unwrap();
    let log = store
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .map(|envelope| envelope.event)
        .collect();
    let messages = kernel::request::projection_to_messages(
        &AgentName::new(kernel::AGENT),
        &store.read_projection_shared(session).await.unwrap(),
        &model,
        &kernel::request::AttachmentData::new(),
    );
    Run {
        provider,
        log,
        finish,
        messages,
    }
}

fn tool_ctx(workdir: &Path, cancel: &CancellationToken) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        operation: hya_tool::ToolOperation::from_tool_call(ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        skills: hya_tool::SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        agents: Arc::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        workdir: workdir.to_path_buf(),
        roots: vec![workdir.to_path_buf()],
        cancel: cancel.clone(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

/// A stream that only reasons, without end.
fn thinks(from: Duration, text: &'static str) -> Script {
    Box::new(move |turn| {
        let part = PartId::new();
        paced(
            turn.reasoning(from, part, text, false)
                .into_iter()
                .chain(turn.think_on(from, part, " more")),
        )
    })
}

/// The text of each user message in `messages`.
fn user_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User { parts, .. } => Some(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

fn cut_notices(log: &[Event]) -> Vec<&str> {
    log.iter()
        .filter_map(|event| match event {
            Event::TextDelta { delta, .. }
                if delta.starts_with(REASONING_DEADLINE_NOTICE_PREFIX) =>
            {
                Some(delta.as_str())
            }
            _ => None,
        })
        .collect()
}

/// Scenario A: the first round only reasons across WrapUp and is cut; the
/// next round of the same turn gets the cut notice, with the WrapUp notice
/// and where the reasoning stopped, once, and answers.
async fn wrap_up_cut_then_answer() {
    let run = bare_turn(
        SessionId::new(),
        200_000,
        vec![
            thinks(secs(0), "the schema needs a version field"),
            Box::new(|turn| paced(turn.text(WRAP_UP, "done").into_iter())),
        ],
    )
    .await;
    assert_eq!(run.finish, FinishReason::Stop);
    let requests = run.provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let texts = user_texts(&requests[1].messages);
    let notice = texts.last().unwrap();
    assert!(
        notice.starts_with(REASONING_DEADLINE_NOTICE_PREFIX),
        "{notice}"
    );
    assert!(notice.contains("\n\nHEADLESS RUN — TIME: 5"), "{notice}");
    assert!(
        notice.contains("the schema needs a version field more more") && notice.ends_with(" more"),
        "{notice}"
    );
    // Before the next round, nothing repeated the stage or the clock.
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.contains("HEADLESS RUN"))
            .count(),
        1,
        "{texts:?}"
    );
    assert_eq!(cut_notices(&run.log), vec![notice.as_str()]);
    let steps: Vec<_> = run
        .log
        .iter()
        .filter_map(|event| match event {
            Event::StepFinished { step, finish, .. } => Some((*step, *finish)),
            _ => None,
        })
        .collect();
    assert_eq!(
        steps,
        vec![(0, FinishReason::Stop), (1, FinishReason::Stop)]
    );
    assert!(run.log.iter().any(|event| matches!(
        event,
        Event::TextReplace { text, .. } if text == "done"
    )));
}

/// Scenario B: a session already told of WrapUp is cut at Final; the next
/// round reasons past Final unhindered and answers.
async fn final_cut_is_not_repeated() {
    let session = SessionId::new();
    deadline::current()
        .unwrap()
        .mark_notified(session, Stage::WrapUp);
    let run = bare_turn(
        session,
        200_000,
        vec![
            thinks(secs(0), "checking the last case"),
            Box::new(|turn| {
                let part = PartId::new();
                let mut events = turn.reasoning(FINAL, part, "one more check", false);
                events.extend(turn.think_on(FINAL, part, ".").take(8));
                events.extend(turn.text(FINAL + secs(45), "final answer"));
                paced(events.into_iter())
            }),
        ],
    )
    .await;
    assert_eq!(run.finish, FinishReason::Stop);
    assert!(deadline::current().unwrap().elapsed() >= FINAL + secs(45));
    let requests = run.provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let texts = user_texts(&requests[1].messages);
    let notice = texts.last().unwrap();
    assert!(
        notice.starts_with(REASONING_DEADLINE_NOTICE_PREFIX),
        "{notice}"
    );
    assert!(notice.contains("\n\nHEADLESS RUN — FINAL 10"), "{notice}");
    assert_eq!(cut_notices(&run.log), vec![notice.as_str()]);
    assert!(run.log.iter().any(|event| matches!(
        event,
        Event::TextReplace { text, .. } if text == "final answer"
    )));
}

/// Scenario C: the rewrite before the round after a cut compacts the
/// context (a step cut through the cut reasoning); the notice still
/// reaches that round's request. The first round's reported prompt keeps
/// every later rewrite over the threshold.
async fn compaction_after_a_cut_keeps_its_notice() {
    let run = bare_turn(
        SessionId::new(),
        8_000,
        vec![
            Box::new(|turn| {
                let Turn { session, message } = turn;
                let mut events = turn.tool_call(secs(0), secs(0));
                events.pop();
                events.push((
                    secs(0),
                    Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: FinishReason::ToolCalls,
                        tokens: Some(TokenUsage {
                            input: 7_000,
                            output: 10,
                            ..TokenUsage::default()
                        }),
                        cause: None,
                    },
                ));
                paced(events.into_iter())
            }),
            thinks(secs(0), "plan the next file"),
            Box::new(|turn| paced(turn.text(WRAP_UP, "done").into_iter())),
        ],
    )
    .await;
    assert_eq!(run.finish, FinishReason::Stop);
    let after = |from: usize, found: &dyn Fn(&Event) -> bool| {
        from + run.log[from..].iter().position(found).unwrap()
    };
    let cut = after(0, &|event| {
        matches!(
            event,
            Event::StepFinished {
                step: 1,
                finish: FinishReason::Stop,
                ..
            }
        )
    });
    let compacted = after(cut, &|event| {
        matches!(
            event,
            Event::ContextCompacted {
                step_cut: Some(_),
                ..
            }
        )
    });
    let notice = after(
        compacted,
        &|event| matches!(event, Event::TextDelta { delta, .. } if delta.starts_with(REASONING_DEADLINE_NOTICE_PREFIX)),
    );
    after(notice, &|event| {
        matches!(event, Event::StepStarted { step: 2, .. })
    });
    let requests = run.provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    let Some(Message::User { parts, .. }) = requests[2].messages.last() else {
        panic!("{:?}", requests[2].messages);
    };
    assert!(
        matches!(parts.as_slice(), [Part::Text { text, .. }] if text.starts_with(REASONING_DEADLINE_NOTICE_PREFIX)),
        "{parts:?}"
    );
}

/// A cut step whose reasoning block had closed with replay data (a
/// Responses item), followed by a step that calls a tool or answers with
/// no reasoning of its own: the transcript keeps the reasoning text, but
/// no later request replays the item, which would pair it with that step.
async fn cut_reasoning_is_not_replayed_before_a_later_step(answer: bool) {
    let next: Script = if answer {
        Box::new(|turn| paced(turn.text(WRAP_UP, "the answer").into_iter()))
    } else {
        Box::new(|turn| paced(turn.tool_call(WRAP_UP, WRAP_UP).into_iter()))
    };
    let mut scripts = vec![
        Box::new(|turn: Turn| {
            let Turn { session, message } = turn;
            let part = PartId::new();
            let mut events = turn.reasoning(secs(0), part, "need the schema", false);
            events.push((
                secs(0),
                Event::ReasoningEnd {
                    session,
                    message,
                    part,
                    provider_data: Some(serde_json::json!({
                        "type": "reasoning",
                        "id": "rs_cut",
                        "encrypted_content": "enc_cut",
                    })),
                },
            ));
            // The stream stays open, silent, past WrapUp.
            events.push((
                secs(700),
                Event::ReasoningDelta {
                    session,
                    message,
                    part,
                    delta: String::new(),
                },
            ));
            paced(events.into_iter())
        }) as Script,
        next,
    ];
    if !answer {
        scripts.push(Box::new(|turn| {
            paced(turn.text(WRAP_UP, "done").into_iter())
        }));
    }
    let run = bare_turn(SessionId::new(), 200_000, scripts).await;
    assert_eq!(run.finish, FinishReason::Stop);
    assert_eq!(cut_notices(&run.log).len(), 1);
    let Some(Message::Assistant { parts, .. }) = run
        .messages
        .iter()
        .find(|message| matches!(message, Message::Assistant { .. }))
    else {
        panic!("{:?}", run.messages);
    };
    assert!(
        parts.iter().any(|part| matches!(
            part,
            Part::Reasoning { text, provider_data: None, .. } if text == "need the schema"
        )),
        "{parts:?}"
    );
    let input = hya_provider::encode_input_items(&run.messages, false).unwrap();
    assert!(
        input
            .iter()
            .all(|item| item.get("id") != Some(&serde_json::json!("rs_cut"))),
        "{input:?}"
    );
}
