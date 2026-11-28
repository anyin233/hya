//! Reasoning deadline guard at the round collector boundary.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use hya_core::CoreError;
use hya_core::kernel::{DeadlineCut, Emit, round};
use hya_proto::{
    Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId, ToolCallId, ToolName,
};
use hya_provider::EventStream;
use hya_tool::deadline::{self, Stage};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// WrapUp starts 585 s into a 900 s budget, Final at 792 s.
const BUDGET: Duration = Duration::from_secs(900);
const WRAP_UP: Duration = Duration::from_secs(585);

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
