//! A model stream that has shown only reasoning is stopped at the run's next
//! deadline stage, and the kernel continues with a notice (`docs/kernel.md`).
//!
//! Runs in-process with Tokio's paused clock and thread-scoped deadlines
//! (`deadline::install_scoped`), eliminating per-case child process spawns.

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

/// WrapUp starts 585 s into a 900 s budget, Final at 792 s.
const BUDGET: Duration = Duration::from_secs(900);
const WRAP_UP: Duration = Duration::from_secs(585);
const FINAL: Duration = Duration::from_secs(792);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Hold off Tokio's auto-advance while SQLite runs on its blocking thread.
fn hold_paused_clock() -> tokio::sync::oneshot::Sender<()> {
    let (hold, held) = tokio::sync::oneshot::channel::<()>();
    tokio::task::spawn_blocking(move || held.blocking_recv());
    hold
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

type Script = Box<dyn Fn(Turn) -> EventStream + Send + Sync>;

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
    messages: Vec<Message>,
}

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
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        hya_tool::Rule::new(hya_tool::Action::Read, "*", hya_tool::Mode::Allow),
        hya_tool::Rule::new(hya_tool::Action::Edit, "*", hya_tool::Mode::Allow),
        hya_tool::Rule::new(hya_tool::Action::Bash, "*", hya_tool::Mode::Allow),
        hya_tool::Rule::new(hya_tool::Action::Grep, "*", hya_tool::Mode::Allow),
    ]));
    ToolCtx::new(workdir.to_path_buf(), permission, cancel.clone())
}

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

/// Round-level reasoning guard: per-step cap cut, late untold-stage cut,
/// disarming on text/tool/first-byte/finish, and reasoning grace windows.
#[tokio::test(start_paused = true)]
async fn round_reasoning_guard_cuts_and_disarms() {
    let _hold = hold_paused_clock();

    // 1. Reasoning-only stream hits per-step cap (270 s) and persists open thought.
    {
        let _d = deadline::install_scoped(BUDGET);
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
        assert!((secs(270)..=secs(275)).contains(&elapsed), "{elapsed:?}");
        assert!(round.tool_calls.is_empty());
        assert_eq!(round.finish, FinishReason::Stop);
        let Some(DeadlineCut { stage, reasoning }) = round.deadline_cut else {
            panic!("reasoning-only stream exceeded its cap");
        };
        assert_eq!(stage, None);
        assert_eq!(
            deadline::current()
                .unwrap()
                .next_checkpoint(turn.session)
                .unwrap()
                .1,
            Stage::WrapUp
        );
        let thought = reasoning
            .strip_prefix("plan the parser\n\n")
            .unwrap_or_else(|| panic!("{reasoning}"));
        assert!(
            (53..=54).contains(&thought.len()) && thought.chars().all(|c| c == 'x'),
            "{thought}"
        );
        assert!(log.iter().any(|event| matches!(
            event,
            Event::ReasoningReplace { part, text, .. } if *part == open && text == thought
        )));
    }

    // 2. Text before WrapUp disarms the guard.
    {
        let _d = deadline::install_scoped(BUDGET);
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

    // 3. Tool input starting before WrapUp disarms the guard even if requested after.
    {
        let _d = deadline::install_scoped(BUDGET);
        let turn = Turn::new();
        let mut events = turn.reasoning(secs(0), PartId::new(), "need a file", true);
        events.extend(turn.tool_call(secs(10), secs(700)));
        let (round, _) = collect(turn, events.into_iter()).await;
        assert!(deadline::current().unwrap().elapsed() >= secs(700));
        assert_eq!(round.deadline_cut, None);
        assert_eq!(round.tool_calls.len(), 1);
    }

    // 4. Waiting for first byte (no reasoning before text) is never cut.
    {
        let _d = deadline::install_scoped(BUDGET);
        let turn = Turn::new();
        let mut events = turn.text(secs(600), "answer");
        events.push(turn.finished(secs(600), FinishReason::Stop));
        let (round, _) = collect(turn, events.into_iter()).await;
        assert!(deadline::current().unwrap().elapsed() >= secs(600));
        assert_eq!(round.deadline_cut, None);
    }

    // 5. Short step straddling WrapUp or starting after an untold stage gets reasoning grace.
    for from in [WRAP_UP - secs(5), secs(600)] {
        let _d = deadline::install_scoped(BUDGET);
        let turn = Turn::new();
        let part = PartId::new();
        let mut events = turn.reasoning(from, part, "look up the format", false);
        events.extend(turn.think_on(from, part, " then").take(3));
        events.extend(turn.tool_call(from + secs(15), from + secs(20)));
        let (round, _) = collect(turn, events.into_iter()).await;
        assert_eq!(round.deadline_cut, None, "from={from:?}");
        assert_eq!(round.tool_calls.len(), 1, "from={from:?}");
    }

    // 6. Long step starting after an untold WrapUp is cut after the 27 s grace.
    {
        let _d = deadline::install_scoped(BUDGET);
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
        assert_eq!(stage, Some(Stage::WrapUp));
        assert_eq!(
            reasoning,
            format!("compare the options{}", " again".repeat(5))
        );
    }

    // 7. A finished step on an open stream is not cut past WrapUp.
    {
        let _d = deadline::install_scoped(BUDGET);
        let turn = Turn::new();
        let part = PartId::new();
        let Turn { session, message } = turn;
        let mut events = turn.reasoning(secs(0), part, "out of room", false);
        events.push(turn.finished(secs(100), FinishReason::Length));
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
}

/// Bare-turn reasoning cuts at WrapUp (once, with notice + thought tail) and at
/// Final (unrepeated on the follow-up round).
#[tokio::test(start_paused = true)]
async fn bare_turn_reasoning_cut_wrap_up_and_final() {
    let _hold = hold_paused_clock();

    // Scenario A: WrapUp cut then answer.
    {
        let _d = deadline::install_scoped(BUDGET);
        let run = bare_turn(
            SessionId::new(),
            200_000,
            vec![
                thinks(secs(500), "the schema needs a version field"),
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
            notice.contains("the schema needs a version field more more")
                && notice.ends_with(" more"),
            "{notice}"
        );
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

    // Scenario B: already told of WrapUp -> cut at Final, follow-up reasons unhindered.
    {
        let _d = deadline::install_scoped(BUDGET);
        let session = SessionId::new();
        deadline::current()
            .unwrap()
            .mark_notified(session, Stage::WrapUp);
        let run = bare_turn(
            session,
            200_000,
            vec![
                thinks(secs(730), "checking the last case"),
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
}

/// Compaction after a cut keeps its notice, and cut reasoning's replay item is
/// stripped before a later tool or text step.
#[tokio::test(start_paused = true)]
async fn bare_turn_reasoning_cut_compaction_and_replay() {
    let _hold = hold_paused_clock();

    // Scenario C: compaction after a cut still delivers the cut notice to the next request.
    {
        let _d = deadline::install_scoped(BUDGET);
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

    // Cut reasoning with Responses provider_data is kept as text but never replayed.
    for answer in [false, true] {
        let _d = deadline::install_scoped(BUDGET);
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
}
