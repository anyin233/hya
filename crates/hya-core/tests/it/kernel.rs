//! Kernel invariants (ADR-0033, plan §5-A): pairing, failures and
//! cancellation as observations, and the rewrite primitive.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hya_core::CoreError;
use hya_core::kernel::{self, StoreEmit, View, request::AttachmentData};
use hya_proto::context::tool_result_text;
use hya_proto::{
    AgentName, CompactionStrategy, Envelope, Event, FileState, FinishReason, Message, MessageId,
    ModelRef, Part, PartId, Projection, Role, SessionId, SessionKind, TokenUsage, ToolPartState,
};
use hya_provider::{
    AnthropicMessagesProtocol, COMPACT_CONTEXT_MARKER, Capabilities, CompletionRequest,
    EventStream, FakeProvider, FakeStep, OpenAiChatProtocol, Protocol, Provider, ProviderError,
    ProviderRouter, ReasoningEffort, ReasoningReplayPolicy,
};
use hya_store::SessionStore;
use hya_tool::deadline::{self, Stage};
use hya_tool::{
    Action, Mode, ModelWindow, PermissionPlane, PermissionRules, Pressure, RewriteCtx, Rewriter,
    Rule, SessionLog, Tool, ToolCtx, ToolError, Transcript,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;

#[test]
fn bare_deadline_notices() {
    std::thread::scope(|scope| {
        for case in ["final", "wrap-up-above", "wrap-up-below", "900", "1800"] {
            scope.spawn(move || run_deadline_test("kernel::bare_deadline_notices_child", case));
        }
    });
}

#[test]
fn bare_deadline_clock() {
    run_deadline_test("kernel::bare_deadline_clock_child", "clock");
}

fn run_deadline_test(child: &str, case: &str) {
    // Install the process-wide clock in a child so no other kernel test sees it.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", child, "--nocapture"])
        .env("HYA_BARE_DEADLINE_TEST", case)
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn bare_deadline_notices_child() {
    let (budget, cutoff, scenario) = match std::env::var("HYA_BARE_DEADLINE_TEST").as_deref() {
        Ok("final") => (
            40,
            180,
            Some((Stage::Final, false, "HEADLESS RUN — FINAL ")),
        ),
        Ok("wrap-up-above") => (
            380,
            180,
            Some((Stage::WrapUp, true, "HEADLESS RUN — TIME: ")),
        ),
        Ok("wrap-up-below") => (
            380,
            180,
            Some((Stage::WrapUp, false, "HEADLESS RUN — TIME: ")),
        ),
        Ok("900") => (900, 216, None),
        Ok("1800") => (1800, 432, None),
        _ => return,
    };
    let cutoff = Duration::from_secs(cutoff);
    let Some((stage, send_check, prefix)) = scenario else {
        deadline::install(Duration::from_secs(budget));
        assert_eq!(2 * deadline::current().unwrap().final_window(), cutoff);
        return;
    };
    let fx = fixture(
        "deadline-notice",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            call("todo", json!({"ops": []})),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    deadline::install(Duration::from_secs(budget));
    let deadline = deadline::current().unwrap();
    assert_eq!(2 * deadline.final_window(), cutoff);
    if stage == Stage::WrapUp {
        // The eligible case starts with the wrap-up stage, as far above the
        // cutoff as that stage allows; the other starts just below the cutoff.
        let remaining = if send_check {
            deadline.wrap_up_window()
        } else {
            cutoff - Duration::from_secs(5)
        };
        tokio::time::sleep(deadline.remaining().saturating_sub(remaining)).await;
    }
    assert_eq!(deadline.stage(), stage);
    assert_eq!(
        fx.turn("write the deliverable", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let log = fx.log().await;
    assert_eq!(deadline.stage(), stage);
    assert_eq!(deadline.remaining() > cutoff, send_check);
    assert_eq!(checks_logged(&log), usize::from(send_check));
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 3 + usize::from(send_check));
    if send_check {
        assert!(requests[3].messages.last().is_some_and(is_check));
    }
    let Some(Message::User { parts, .. }) = requests[0].messages.last() else {
        panic!("expected the deadline notice at the end of the first request");
    };
    let [Part::Text { text, .. }] = parts.as_slice() else {
        panic!("expected a single deadline notice");
    };
    assert!(text.starts_with(prefix), "{text}");
    if stage == Stage::Final {
        let left = text
            .strip_prefix(prefix)
            .and_then(|rest| rest.split_once("s: "))
            .and_then(|(seconds, _)| seconds.parse::<u64>().ok())
            .expect("Final notice with seconds left");
        assert!((30..40).contains(&left), "{text}");
        assert!(text.contains("of 40s used. Write any missing deliverable"));
        assert!(
            text.ends_with(
                "then end your turn. Do not edit a file you have already delivered in place: \
                 if it needs a change, change a copy, run the check the file passed, and replace \
                 the file only if the copy passes. Start only commands that finish well within \
                 the time left."
            ),
            "{text}"
        );
    } else {
        assert!(text.contains(&format!("of {budget}s used")), "{text}");
        assert!(
            text.contains("the request's own checks, then finish"),
            "{text}"
        );
    }
    assert!(
        requests
            .iter()
            .all(|req| req.system.as_deref() == Some(fx.system.as_str()))
    );
    for req in &requests[1..] {
        assert!(!format!("{:?}", req.messages).contains(prefix));
    }
    let logged: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::TextDelta { delta, .. } if delta.starts_with(prefix) => Some(delta),
            _ => None,
        })
        .collect();
    assert_eq!(logged, vec![text]);
}

#[tokio::test]
async fn bare_deadline_clock_child() {
    if std::env::var_os("HYA_BARE_DEADLINE_TEST").is_none() {
        return;
    }
    let fx = fixture(
        "deadline-clock",
        FakeProvider::scripted_turns(vec![
            text("ready"),
            call("todo", json!({"ops": []})),
            text("first update"),
            text("second update"),
        ]),
    )
    .await;
    let tenth = Duration::from_secs(3);
    deadline::install(tenth * 10);
    let deadline = deadline::current().unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(fx.turn("start", &cancel).await.unwrap(), FinishReason::Stop);
    for threshold in [tenth, tenth * 2] {
        tokio::time::sleep(threshold.saturating_sub(deadline.elapsed())).await;
        assert_eq!(
            fx.turn("continue", &cancel).await.unwrap(),
            FinishReason::Stop
        );
    }

    let requests = fx.provider.requests();
    let clocks: Vec<Vec<_>> = requests
        .iter()
        .map(|req| {
            req.messages
                .iter()
                .flat_map(|message| match message {
                    Message::User { parts, .. } => parts.as_slice(),
                    _ => &[],
                })
                .filter_map(|part| match part {
                    Part::Text { id, text } if text.starts_with("HEADLESS RUN — clock: ") => {
                        Some((id, text))
                    }
                    _ => None,
                })
                .collect()
        })
        .collect();
    let [before, first, same, next] = clocks.as_slice() else {
        panic!("expected four model rounds");
    };
    assert!(before.is_empty());
    assert_eq!(first.len(), 1, "first tenth must reach the model");
    assert!(
        same.is_empty(),
        "the clock line leaves the request once the assistant has responded, and the same tenth logs no other"
    );
    assert_eq!(next.len(), 1, "the next tenth must reach the model");
    assert_ne!(next, first);
    assert!(assistant_parts(&requests[2]).any(|part| matches!(
        part,
        Part::Tool { name, state: ToolPartState::Completed { output, .. }, .. }
            if name.as_str() == "todo" && output == &json!(hya_proto::context::render_todos(&[]))
    )));
    assert!(
        requests
            .iter()
            .all(|req| req.system.as_deref() == Some(fx.system.as_str()))
    );

    let log = fx.log().await;
    let logged: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::TextDelta { part, delta, .. } if delta.starts_with("HEADLESS RUN — clock: ") => {
                Some((part, delta))
            }
            _ => None,
        })
        .collect();
    assert_eq!(logged, vec![first[0], next[0]]);
}

struct Fixture {
    watches: kernel::WorkspaceWatches,
    store: SessionStore,
    provider: Arc<FakeProvider>,
    router: ProviderRouter,
    model: ModelRef,
    session: SessionId,
    dir: PathBuf,
    system: String,
    tools: Vec<Arc<dyn Tool>>,
}

async fn fixture(name: &str, provider: FakeProvider) -> Fixture {
    let dir = std::env::temp_dir().join(format!("hya-kernel-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = SessionStore::connect_memory().await.unwrap();
    fixture_with_store(dir, provider, store).await
}

async fn fixture_with_store(dir: PathBuf, provider: FakeProvider, store: SessionStore) -> Fixture {
    let provider = Arc::new(provider);
    let router = ProviderRouter::new().with(provider.clone());
    let model = ModelRef::new("fake/model");
    let session = SessionId::new();
    store
        .append_events(
            session,
            vec![Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new(kernel::AGENT),
                model: model.clone(),
                workdir: dir.display().to_string(),
                project: None,
                kind: SessionKind::Project,
            }],
        )
        .await
        .unwrap();
    Fixture {
        watches: Default::default(),
        system: kernel::prompt::system_prompt(&dir),
        store,
        provider,
        router,
        model,
        session,
        dir,
        tools: kernel::tools::builtin(),
    }
}

impl Fixture {
    /// Write a user message and an assistant message, run the kernel over
    /// it, and finish the assistant message.
    async fn turn(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        self.turn_with_publish(prompt, cancel, &|_| {}).await
    }

    /// Write a user message and start an assistant message after it.
    async fn begin(&self, prompt: &str) -> MessageId {
        let session = self.session;
        let user = MessageId::new();
        let part = PartId::new();
        let message = MessageId::new();
        self.store
            .append_events(
                session,
                vec![
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
                        delta: prompt.to_string(),
                    },
                    Event::TextEnd {
                        session,
                        message: user,
                        part,
                    },
                    Event::MessageFinished {
                        session,
                        message: user,
                        role: Role::User,
                        finish: FinishReason::Stop,
                        tokens: None,
                        cause: None,
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
        message
    }

    async fn turn_with_publish(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
        publish: &(dyn Fn(Envelope) + Send + Sync),
    ) -> Result<FinishReason, CoreError> {
        let session = self.session;
        let message = self.begin(prompt).await;
        let emit = StoreEmit {
            store: &self.store,
            publish,
        };
        let ctx = tool_ctx(&self.dir, cancel);
        let finish = kernel::run_with_watches(
            kernel::Run {
                store: &self.store,
                emit: &emit,
                provider: &self.router,
                model: &self.model,
                system: &self.system,
                tools: &self.tools,
                ctx: &ctx,
                session,
                message,
                cancel,
            },
            &self.watches,
        )
        .await?;
        self.store
            .append_events(
                session,
                vec![Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish,
                    tokens: None,
                    cause: None,
                }],
            )
            .await
            .unwrap();
        Ok(finish)
    }

    async fn log(&self) -> Vec<Event> {
        self.store
            .replay(self.session)
            .await
            .unwrap()
            .into_iter()
            .map(|envelope| envelope.event)
            .collect()
    }
}

fn tool_ctx(workdir: &Path, cancel: &CancellationToken) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
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

fn call(name: &str, input: serde_json::Value) -> Vec<FakeStep> {
    vec![FakeStep::ToolCall {
        name: name.to_string(),
        input,
    }]
}

fn text(body: &str) -> Vec<FakeStep> {
    vec![FakeStep::Text(body.to_string())]
}

/// Every result answers an earlier call; every call has its result before
/// the next step starts (and before the log ends).
fn assert_paired(log: &[Event]) {
    let mut requested = HashSet::new();
    let mut open = Vec::new();
    for event in log {
        match event {
            Event::ToolCallRequested { call, .. } => {
                requested.insert(*call);
                open.push(*call);
            }
            Event::ToolResult { call, .. } | Event::ToolError { call, .. } => {
                assert!(
                    requested.contains(call),
                    "result for unrequested call {call}"
                );
                open.retain(|open| open != call);
            }
            Event::StepStarted { .. } => {
                assert!(
                    open.is_empty(),
                    "calls {open:?} have no result before the next step"
                );
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "calls {open:?} have no result");
}

fn tool_errors(log: &[Event]) -> Vec<&str> {
    log.iter()
        .filter_map(|event| match event {
            Event::ToolError { message_text, .. } => Some(message_text.as_str()),
            _ => None,
        })
        .collect()
}

fn assistant_parts(request: &CompletionRequest) -> impl Iterator<Item = &Part> {
    request.messages.iter().flat_map(|message| match message {
        Message::Assistant { parts, .. } => parts.as_slice(),
        _ => &[],
    })
}

fn part_ids(request: &CompletionRequest) -> HashSet<PartId> {
    assistant_parts(request)
        .map(|part| match part {
            Part::Text { id, .. }
            | Part::Reasoning { id, .. }
            | Part::Media { id, .. }
            | Part::Tool { id, .. } => *id,
        })
        .collect()
}

#[tokio::test]
async fn a1_tool_results_pair_with_calls() {
    let fx = fixture(
        "a1",
        FakeProvider::scripted_turns(vec![
            call("write", json!({"path": "out/a.txt", "content": "hello"})),
            call("bash", json!({"command": "cat out/a.txt"})),
            text("done"),
        ]),
    )
    .await;
    let finish = fx.turn("write then cat", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    let log = fx.log().await;
    assert_paired(&log);
    let outputs: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        outputs,
        vec![
            json!({"bytes": 5}),
            json!({"exit_code": 0, "output": "hello\n[exit code: 0]"})
        ]
    );
}

const CHECK: &str = "[hya] Completion check, sent once before this run ends.";

fn is_check(message: &Message) -> bool {
    matches!(message, Message::User { parts, .. }
        if matches!(parts.as_slice(), [Part::Text { text, .. }] if text.starts_with(CHECK)))
}

fn checks_logged(log: &[Event]) -> usize {
    log.iter()
        .filter(|event| matches!(event, Event::TextDelta { delta, .. } if delta.starts_with(CHECK)))
        .count()
}

/// After a tool call, the first answer gets the completion check; the second
/// answer ends the run.
#[tokio::test]
async fn completion_check_is_sent_once_after_a_tool_call() {
    let fx = fixture(
        "completion-check",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            text("draft"),
            call("todo", json!({"ops": []})),
            text("final"),
            text("unreached"),
        ]),
    )
    .await;
    let finish = fx.turn("do it", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 4);
    assert_eq!(checks_logged(&fx.log().await), 1);
    let projection = fx.store.read_projection_shared(fx.session).await.unwrap();
    let reply = projection
        .session
        .messages
        .iter()
        .rfind(|message| message.role == Role::Assistant)
        .unwrap();
    let last_text = reply.parts.iter().rev().find_map(|part| match part {
        hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(last_text, Some("final"));
}

const EMPTY_TURN: &str = "[hya] Your last turn ended with thinking only";

fn notices_logged(log: &[Event]) -> Vec<&str> {
    log.iter()
        .filter_map(|event| match event {
            Event::TextDelta { delta, .. } if delta.starts_with(EMPTY_TURN) => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

/// A round that only reasoned, as an upstream output cap leaves it.
fn thinking_only(thought: &str, output: Option<u64>) -> Vec<FakeStep> {
    let mut steps = vec![FakeStep::Reasoning(thought.into())];
    if let Some(output) = output {
        steps.push(FakeStep::Usage(TokenUsage {
            output,
            ..TokenUsage::default()
        }));
    }
    steps.push(FakeStep::Finish(FinishReason::Stop));
    steps
}

async fn last_answer(fx: &Fixture) -> Option<String> {
    let projection = fx.store.read_projection_shared(fx.session).await.unwrap();
    let reply = projection
        .session
        .messages
        .iter()
        .rfind(|message| message.role == Role::Assistant)
        .unwrap();
    reply.parts.iter().rev().find_map(|part| match part {
        hya_proto::PartProjection::Text { text, .. } => Some(text.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn a_turn_with_thinking_only_gets_a_notice_and_another_round() {
    let fx = fixture(
        "empty-turn",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            thinking_only("a long thought", Some(65_536)),
            text("draft"),
            text("final"),
        ]),
    )
    .await;
    let finish = fx.turn("do it", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    let log = fx.log().await;
    let notices = notices_logged(&log);
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].contains("It ended after 65536 output tokens."),
        "{}",
        notices[0]
    );
    assert_eq!(
        checks_logged(&log),
        1,
        "the completion check is still sent once"
    );
    assert_eq!(last_answer(&fx).await.as_deref(), Some("final"));

    // The empty step ends, the notice is logged, and the next step starts.
    let empty_end = log
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::StepFinished {
                    step: 1,
                    finish: FinishReason::Stop,
                    ..
                }
            )
        })
        .unwrap();
    assert!(
        matches!(
            &log[empty_end + 1..empty_end + 7],
            [
                Event::MessageStarted { role: Role::System, .. },
                Event::TextStart { .. },
                Event::TextDelta { delta, .. },
                Event::TextEnd { .. },
                Event::MessageFinished { role: Role::System, .. },
                Event::StepStarted { step: 2, .. },
            ] if delta.starts_with(EMPTY_TURN)
        ),
        "{:?}",
        &log[empty_end..]
    );

    // The todo round, the empty turn, the answer that gets the check, the final answer.
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 4);
    let sent_notice = |request: &CompletionRequest| {
        request.messages.iter().any(|message| matches!(message, Message::User { parts, .. }
            if matches!(parts.as_slice(), [Part::Text { text, .. }] if text.starts_with(EMPTY_TURN))))
    };
    assert!(sent_notice(&requests[2]));
    assert!(
        !sent_notice(&requests[3]),
        "the answered notice leaves the request"
    );
    for request in &requests[2..] {
        assert!(assistant_parts(request).any(|part| matches!(
            part,
            Part::Reasoning { text, .. } if text == "a long thought"
        )));
    }
}

#[test]
fn an_empty_turn_in_the_final_stage_gets_the_notice() {
    run_deadline_test(
        "kernel::an_empty_turn_in_the_final_stage_gets_the_notice_child",
        "empty-final",
    );
}

#[tokio::test]
async fn an_empty_turn_in_the_final_stage_gets_the_notice_child() {
    if std::env::var("HYA_BARE_DEADLINE_TEST").as_deref() != Ok("empty-final") {
        return;
    }
    let fx = fixture(
        "empty-turn-final",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            thinking_only("out of time", None),
            text("done"),
        ]),
    )
    .await;
    deadline::install(Duration::from_secs(40));
    assert_eq!(deadline::current().unwrap().stage(), Stage::Final);
    let finish = fx.turn("do it", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 3);
    let log = fx.log().await;
    assert_eq!(notices_logged(&log).len(), 1);
    assert_eq!(
        checks_logged(&log),
        0,
        "no check this close to the deadline"
    );
    assert_eq!(last_answer(&fx).await.as_deref(), Some("done"));
}

#[tokio::test]
async fn a_third_empty_turn_in_a_row_ends_the_run() {
    let fx = fixture(
        "empty-turn-limit",
        FakeProvider::scripted_turns(vec![
            thinking_only("one", None),
            thinking_only("two", None),
            thinking_only("three", None),
            text("unreached"),
        ]),
    )
    .await;
    let finish = fx.turn("do it", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 3);
    let log = fx.log().await;
    let notices = notices_logged(&log);
    assert_eq!(notices.len(), 2);
    assert!(
        notices
            .iter()
            .all(|notice| !notice.contains("output tokens")),
        "no usage, no token sentence: {notices:?}"
    );
    assert_eq!(last_answer(&fx).await, None);
}

#[tokio::test]
async fn a_tool_call_resets_the_empty_turn_count() {
    let fx = fixture(
        "empty-turn-reset",
        FakeProvider::scripted_turns(vec![
            thinking_only("one", None),
            thinking_only("two", None),
            call("todo", json!({"ops": []})),
            thinking_only("three", None),
            thinking_only("four", None),
            text("draft"),
            text("final"),
        ]),
    )
    .await;
    let finish = fx.turn("do it", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 7);
    let log = fx.log().await;
    assert_eq!(notices_logged(&log).len(), 4);
    assert_eq!(checks_logged(&log), 1);
    assert_eq!(last_answer(&fx).await.as_deref(), Some("final"));
}

#[tokio::test]
async fn an_empty_turn_after_the_completion_check_gets_no_second_check() {
    let fx = fixture(
        "empty-turn-after-check",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            text("draft"),
            thinking_only("checking", Some(12)),
            text("final"),
            text("unreached"),
        ]),
    )
    .await;
    let finish = fx.turn("do it", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 4);
    let log = fx.log().await;
    assert_eq!(checks_logged(&log), 1);
    assert_eq!(notices_logged(&log).len(), 1);
    assert_eq!(last_answer(&fx).await.as_deref(), Some("final"));
}

#[tokio::test]
async fn whitespace_only_text_is_an_empty_turn() {
    let fx = fixture(
        "empty-turn-whitespace",
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::Text(" \n\t ".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            text("done"),
        ]),
    )
    .await;
    let finish = fx.turn("do it", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 2);
    assert_eq!(notices_logged(&fx.log().await).len(), 1);
    assert_eq!(last_answer(&fx).await.as_deref(), Some("done"));
}

#[tokio::test]
async fn a_logged_completion_check_keeps_its_behavior_after_the_wording_changes() {
    let fx = fixture(
        "check-replay",
        FakeProvider::scripted_turns(vec![
            text("draft"),
            call("todo", json!({"ops": []})),
            text("final"),
        ]),
    )
    .await;
    let cancel = CancellationToken::new();
    fx.turn("do it", &cancel).await.unwrap();

    let old_check = "[hya] Completion check, sent once before this run ends. Compare the result with the request itself, not with your memory of it.
1. Re-read the request. List what it states literally: paths and file names, exact strings and output formats, commands and flags, values and counts, and anything it says is unknown or must not be assumed.
2. For each item, look at the result as it is now (the file on disk, the output of the real command) and compare it with the request's wording, character for character where the wording is exact. A check that only shows what you expected to see (a pattern you wrote, a substring, a summary) does not count.
3. If something differs, fix it and check again. If everything already matches, change nothing.
Then give your final answer.";
    fx.store
        .append_events(
            fx.session,
            kernel::round::system_message_events(fx.session, MessageId::new(), old_check.into()),
        )
        .await
        .unwrap();
    let projection = fx.store.read_projection_shared(fx.session).await.unwrap();
    let messages = kernel::request::projection_to_messages(
        &AgentName::new(kernel::AGENT),
        &projection,
        &fx.model,
        &AttachmentData::new(),
    );
    let rendered_as_user = messages.last().is_some_and(is_check);

    fx.turn("continue", &cancel).await.unwrap();
    let requests = fx.provider.requests();
    assert_eq!(
        (
            rendered_as_user,
            requests.len(),
            checks_logged(&fx.log().await)
        ),
        (true, 3, 1)
    );
    assert!(
        requests[1..]
            .iter()
            .all(|req| req.messages.iter().all(|msg| {
                !is_check(msg)
                    && !matches!(msg, Message::System { content, .. } if content == old_check)
            }))
    );
}

/// Like a deadline stage notice, the check is the last message of the next
/// request and leaves the request once the model has responded to it.
#[tokio::test]
async fn the_completion_check_leaves_the_request_after_the_next_step() {
    let fx = fixture(
        "check-transient",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            text("draft"),
            call("todo", json!({"ops": []})),
            text("final"),
        ]),
    )
    .await;
    fx.turn("do it", &CancellationToken::new()).await.unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(!requests[1].messages.iter().any(is_check));
    let Some(Message::User { parts, .. }) = requests[2].messages.last() else {
        panic!("expected the completion check at the end of the request");
    };
    let [Part::Text { text, .. }] = parts.as_slice() else {
        panic!("expected the completion check in a single text part");
    };
    assert!(text.starts_with(CHECK));
    assert_eq!(
        text.lines().nth(2),
        Some(concat!(
            "2. For each item, look at the result as it is now (the file on disk, the output of the real command) ",
            "and compare it with the request's wording. Where the wording is exact, print the actual text and read ",
            "it against the request's text, character for character. A program or pattern you wrote to do the ",
            "comparison does not count, and neither does a substring or a summary. Where the request says a value ",
            "is unknown, try the result with a value other than the one you can see."
        ))
    );
    assert!(!requests[3].messages.iter().any(is_check));
    let parts: Vec<_> = assistant_parts(&requests[3]).collect();
    assert!(matches!(
        parts.as_slice(),
        [
            Part::Tool { .. },
            Part::Text { text, .. },
            Part::Tool { .. },
        ] if text == "draft"
    ));
}

#[tokio::test]
async fn a_run_without_tool_calls_ends_without_the_completion_check() {
    let fx = fixture(
        "no-completion-check",
        FakeProvider::scripted_turns(vec![text("hi"), text("unreached")]),
    )
    .await;
    let finish = fx.turn("say hi", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 1);
    assert_eq!(checks_logged(&fx.log().await), 0);
}

#[tokio::test]
async fn a_call_of_an_unknown_tool_ends_without_the_completion_check() {
    let fx = fixture(
        "unknown-tool-no-check",
        FakeProvider::scripted_turns(vec![
            call("nope", json!({})),
            text("done"),
            text("unreached"),
        ]),
    )
    .await;
    let finish = fx.turn("go", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 2);
    assert_eq!(checks_logged(&fx.log().await), 0);
}

#[tokio::test]
async fn an_error_finish_ends_without_the_completion_check() {
    let fx = fixture(
        "error-finish-no-check",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            vec![
                FakeStep::Text("filtered".to_string()),
                FakeStep::Finish(FinishReason::Error),
            ],
            text("unreached"),
        ]),
    )
    .await;
    let finish = fx.turn("go", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Error);
    assert_eq!(fx.provider.requests().len(), 2);
    assert_eq!(checks_logged(&fx.log().await), 0);
}

/// A cancel that fires as the answer's step finishes ends the run as before,
/// with no check logged that no request would carry.
#[tokio::test]
async fn a_cancel_as_the_answer_ends_logs_no_completion_check() {
    let fx = fixture(
        "cancel-no-check",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            text("done"),
            text("unreached"),
        ]),
    )
    .await;
    let cancel = CancellationToken::new();
    let trip = cancel.clone();
    let finish = fx
        .turn_with_publish("go", &cancel, &move |envelope| {
            if matches!(envelope.event, Event::StepFinished { step: 1, .. }) {
                trip.cancel();
            }
        })
        .await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 2);
    assert_eq!(checks_logged(&fx.log().await), 0);
}

#[tokio::test]
async fn every_bare_request_carries_the_configured_reasoning_default() {
    let fx = fixture(
        "reasoning-default",
        FakeProvider::scripted_turns(vec![
            call("bash", json!({"command": "true"})),
            text("done"),
            text("checked"),
        ])
        .with_reasoning_default(ReasoningEffort::Max),
    )
    .await;
    let finish = fx.turn("run true", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    let efforts: Vec<_> = fx.provider.requests().iter().map(|r| r.reasoning).collect();
    assert_eq!(efforts, vec![Some(ReasoningEffort::Max); 3]);
}

#[tokio::test]
async fn a2_failures_are_observations() {
    let fx = fixture(
        "a2",
        FakeProvider::scripted_turns(vec![
            call("no_such_tool", json!({})),
            call("edit", json!({"path": "a.txt", "new": "x"})),
            call("read", json!({"path": "missing.txt"})),
            text("next"),
            text("checked"),
        ]),
    )
    .await;
    let finish = fx.turn("fail three ways", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    let log = fx.log().await;
    assert_paired(&log);
    let errors = tool_errors(&log);
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert_eq!(errors[0], "unknown tool: no_such_tool");
    assert!(errors[1].contains("old"), "{}", errors[1]);
    // The next round runs as usual and sees all three failures.
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 5);
    let failed = assistant_parts(&requests[3])
        .filter(|part| {
            matches!(
                part,
                Part::Tool {
                    state: ToolPartState::Error { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(failed, 3);
    assert!(
        log.iter()
            .any(|event| matches!(event, Event::TextReplace { text, .. } if text == "next"))
    );
}

/// The input a provider decoder records for a call whose argument text is
/// `args` (the stream ends at the output limit).
fn decoded_input(args: &str) -> serde_json::Value {
    let args = serde_json::to_string(args).unwrap();
    let mut decoder = OpenAiChatProtocol.decoder(SessionId::new(), MessageId::new(), None);
    let mut events = decoder
        .push(&format!(
            r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"c","function":{{"name":"x","arguments":{args}}}}}]}},"finish_reason":"length"}}]}}"#
        ))
        .unwrap();
    events.extend(decoder.finish().unwrap());
    events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallRequested { input, .. } => Some(input),
            _ => None,
        })
        .unwrap()
}

/// A tool whose arguments are all optional; counts its runs.
#[derive(Default)]
struct Optional(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl Tool for Optional {
    fn name(&self) -> &str {
        "optional"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        hya_proto::ToolSchema {
            name: hya_proto::ToolName::new("optional"),
            description: "Takes only optional arguments.".to_string(),
            input_schema: json!({"type": "object", "properties": {"note": {"type": "string"}}}),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        _ctx: &ToolCtx,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(json!("ran"))
    }
}

/// A call whose argument text is not valid JSON runs no tool: the model's
/// next request carries a failed result with the parser error, and the run
/// goes on.
#[tokio::test]
async fn a_call_with_invalid_argument_json_fails_without_running() {
    let mut fx = fixture(
        "invalid-args",
        FakeProvider::scripted_turns(vec![
            call("bash", decoded_input(r#"{"command":"ec"#)),
            call("optional", decoded_input(r#"{"note":"a"}}"#)),
            text("done"),
        ]),
    )
    .await;
    let optional = Arc::new(Optional::default());
    fx.tools.push(optional.clone());
    let finish = fx
        .turn("call with broken arguments", &CancellationToken::new())
        .await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(optional.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    let log = fx.log().await;
    assert_paired(&log);
    let requests = fx.provider.requests();
    for (request, error) in [
        (
            &requests[1],
            "EOF while parsing a string at line 1 column 14",
        ),
        (&requests[2], "trailing characters at line 1 column 13"),
    ] {
        let message = assistant_parts(request)
            .filter_map(|part| match part {
                Part::Tool {
                    state: ToolPartState::Error { message, .. },
                    ..
                } => Some(message.as_str()),
                _ => None,
            })
            .last()
            .unwrap();
        assert_eq!(
            message,
            format!(
                "arguments are not valid JSON: {error}. Nothing was run; call the tool \
                 again with its arguments as one valid JSON object."
            )
        );
        AnthropicMessagesProtocol.encode(request).unwrap();
    }
    assert!(
        log.iter()
            .any(|event| matches!(event, Event::TextReplace { text, .. } if text == "done"))
    );
}

/// One provider call in the flaky fixture.
enum Attempt {
    Play,
    Open(ProviderError),
    Stream(Vec<FakeStep>, ProviderError),
    /// The whole script, then the error.
    Break(Vec<FakeStep>, ProviderError),
}

/// A provider whose stream calls follow `failures`, one [`Attempt`] each, and
/// then `fake`.
struct Flaky {
    fake: FakeProvider,
    failures: std::sync::Mutex<std::collections::VecDeque<Attempt>>,
    requests: std::sync::Mutex<Vec<CompletionRequest>>,
    clock: RetryClock,
}

#[async_trait::async_trait]
impl Provider for Flaky {
    fn id(&self) -> &str {
        self.fake.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.fake.capabilities(model)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.clock.finish_wait();
        self.requests.lock().unwrap().push(req.clone());
        let attempt = self.failures.lock().unwrap().pop_front();
        let (partial, error, cut) = match attempt {
            None | Some(Attempt::Play) => return self.fake.stream(req, session, message).await,
            Some(Attempt::Open(error)) => {
                self.clock.pause();
                return Err(error);
            }
            Some(Attempt::Stream(partial, error)) => (partial, error, true),
            Some(Attempt::Break(script, error)) => (script, error, false),
        };
        let mut events = FakeProvider::materialize(&partial, session, message);
        if cut {
            events.pop();
        }
        let items = events.into_iter().map(Ok).chain([Err(error)]);
        Ok(Box::pin(futures::stream::iter(items)))
    }
}

fn server_error(message: &str) -> ProviderError {
    ProviderError::HttpStatus {
        status: 500,
        message: format!("in-stream error (server_error): {message}"),
        retry_after: None,
    }
}

/// A fixture whose provider is a [`Flaky`] over `fake` and `failures`.
async fn flaky_fixture(
    name: &str,
    fake: Vec<Vec<FakeStep>>,
    failures: Vec<Attempt>,
) -> (Fixture, Arc<Flaky>) {
    let mut fx = fixture(name, FakeProvider::scripted_turns(Vec::new())).await;
    let flaky = Arc::new(Flaky {
        fake: FakeProvider::scripted_turns(fake),
        failures: std::sync::Mutex::new(failures.into()),
        requests: std::sync::Mutex::default(),
        clock: RetryClock::default(),
    });
    fx.router = ProviderRouter::new().with(flaky.clone());
    (fx, flaky)
}

#[derive(Clone, Default)]
struct RetryClock(Arc<std::sync::Mutex<RetryClockState>>);

#[derive(Default)]
struct RetryClockState {
    pending: Option<PendingWait>,
    waits: Vec<Duration>,
}

struct PendingWait {
    started: tokio::time::Instant,
    hold_clock: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RetryClock {
    fn pause(&self) {
        tokio::time::pause();
        let (hold_clock, done) = tokio::sync::oneshot::channel();
        // A blocking task holds off Tokio's auto-advance while the retry
        // check reads SQLite, which runs on its own thread.
        tokio::task::spawn_blocking(move || {
            let _ = done.blocking_recv();
        });
        self.0.lock().unwrap().pending = Some(PendingWait {
            started: tokio::time::Instant::now(),
            hold_clock: Some(hold_clock),
        });
    }

    fn finish_wait(&self) {
        let mut state = self.0.lock().unwrap();
        if let Some(wait) = state.pending.take() {
            if wait.hold_clock.is_none() {
                state.waits.push(wait.started.elapsed());
            }
            tokio::time::resume();
        }
    }

    fn assert_waits(&self, seconds: &[u64]) {
        let state = self.0.lock().unwrap();
        assert_eq!(state.waits.len(), seconds.len(), "{:?}", state.waits);
        for (&actual, &seconds) in state.waits.iter().zip(seconds) {
            let expected = Duration::from_secs(seconds);
            // Tokio rounds the timer deadline up to a millisecond.
            assert!(
                (expected..=expected + Duration::from_millis(1)).contains(&actual),
                "expected {expected:?}, waited {actual:?}"
            );
        }
    }
}

impl tracing::Subscriber for RetryClock {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.name() == "bare_round_retry"
    }

    fn event(&self, _: &tracing::Event<'_>) {
        // The kernel has checked the durable projection and is about to sleep.
        let mut state = self.0.lock().unwrap();
        let wait = state.pending.as_mut().unwrap();
        drop(wait.hold_clock.take());
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        unreachable!("only the bare_round_retry event is enabled")
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

impl Flaky {
    async fn turn(&self, fx: &Fixture, prompt: &str) -> Result<FinishReason, CoreError> {
        let publish = |envelope: Envelope| {
            if matches!(
                envelope.event,
                Event::StepFinished {
                    finish: FinishReason::Error,
                    ..
                }
            ) {
                self.clock.pause();
            }
        };
        let finish = fx
            .turn_with_publish(prompt, &CancellationToken::new(), &publish)
            .with_subscriber(self.clock.clone())
            .await;
        self.clock.finish_wait();
        finish
    }
}

#[tokio::test]
async fn transient_round_failure_retries_the_same_request() {
    let (fx, flaky) = flaky_fixture(
        "retry-transient",
        vec![
            vec![
                FakeStep::Reasoning("plan the todo".into()),
                FakeStep::ToolCall {
                    name: "todo".into(),
                    input: json!({"ops": []}),
                },
            ],
            text("done"),
            text("checked"),
        ],
        vec![
            Attempt::Play,
            Attempt::Stream(Vec::new(), server_error("upstream unreachable")),
        ],
    )
    .await;
    assert_eq!(
        flaky.turn(&fx, "plan, then answer").await.unwrap(),
        FinishReason::Stop
    );
    flaky.clock.assert_waits(&[5]);

    let requests = flaky.requests.lock().unwrap().clone();
    // The todo round, the failed round, its retry, and the round after the
    // completion check.
    assert_eq!(requests.len(), 4);
    assert_eq!(format!("{:?}", requests[1]), format!("{:?}", requests[2]));
    let last = &requests[3];
    let parts: Vec<_> = assistant_parts(last).collect();
    assert!(
        parts
            .iter()
            .any(|part| matches!(part, Part::Reasoning { text, .. } if text == "plan the todo")),
        "the completed step's reasoning is replayed: {parts:?}"
    );
    assert!(
        parts
            .iter()
            .any(|part| matches!(part, Part::Text { text, .. } if text == "done"))
    );
    let log = fx.log().await;
    let steps: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::StepFinished { step, finish, .. } => Some((*step, *finish)),
            _ => None,
        })
        .collect();
    assert_eq!(steps[1], (1, FinishReason::Error), "{steps:?}");
    assert_eq!(steps[2], (2, FinishReason::Stop), "{steps:?}");
    let views: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::StepStarted { step, view, .. } if [1, 2].contains(step) => view.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(views.len(), 2);
    assert_eq!(
        views[0], views[1],
        "the retry reuses the failed step's view"
    );
}

#[tokio::test]
async fn transient_open_failure_retries_the_same_request() {
    let mut todo = call("todo", json!({"ops": []}));
    todo.push(FakeStep::Finish(FinishReason::ToolCalls));
    let (fx, flaky) = flaky_fixture(
        "retry-open",
        vec![todo, text("done"), text("checked")],
        vec![
            Attempt::Play,
            Attempt::Open(ProviderError::Transport("connection lost".into())),
            Attempt::Open(ProviderError::HttpStatus {
                status: 429,
                message: "rate limited".into(),
                retry_after: None,
            }),
        ],
    )
    .await;
    assert_eq!(
        flaky.turn(&fx, "hi").await.expect("the open is retried"),
        FinishReason::Stop
    );
    flaky.clock.assert_waits(&[5, 10]);
    let requests = flaky.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 5);
    for request in &requests[2..4] {
        assert_eq!(format!("{request:?}"), format!("{:?}", requests[1]));
    }
    let steps: Vec<_> = fx
        .log()
        .await
        .into_iter()
        .filter_map(|event| match event {
            Event::StepStarted { step, .. } => Some((step, None)),
            Event::StepFinished { step, finish, .. } => Some((step, Some(finish))),
            _ => None,
        })
        .collect();
    assert_eq!(
        steps,
        [
            (0, None),
            (0, Some(FinishReason::ToolCalls)),
            (1, None),
            (1, Some(FinishReason::Stop)),
            (2, None),
            (2, Some(FinishReason::Stop)),
        ]
    );
}

#[tokio::test]
async fn the_ninth_open_failure_ends_the_run() {
    let failures = (1..=9)
        .map(|attempt| {
            Attempt::Open(ProviderError::HttpStatus {
                status: 500,
                message: format!("attempt {attempt}"),
                retry_after: None,
            })
        })
        .collect();
    let (fx, flaky) = flaky_fixture("retry-open-limit", vec![text("unreached")], failures).await;
    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("the ninth open fails");
    assert_eq!(error.to_string(), "http status 500: attempt 9");
    flaky.clock.assert_waits(&[5, 10, 20, 40, 60, 60, 60, 60]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 9);
}

#[tokio::test]
async fn non_transient_open_failure_ends_the_run() {
    let (fx, flaky) = flaky_fixture(
        "retry-open-invalid",
        vec![text("unreached")],
        vec![Attempt::Open(ProviderError::HttpStatus {
            status: 400,
            message: "bad request".into(),
            retry_after: None,
        })],
    )
    .await;
    let error = flaky.turn(&fx, "hi").await.expect_err("a 400 ends the run");
    assert_eq!(error.to_string(), "http status 400: bad request");
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_stream_interruption_before_any_part_is_retried() {
    let (fx, flaky) = flaky_fixture(
        "retry-interrupted-stream",
        vec![text("done")],
        vec![
            Attempt::Stream(
                Vec::new(),
                ProviderError::StreamInterrupted("stalled stream: no SSE frame within 300s".into()),
            ),
            Attempt::Stream(Vec::new(), server_error("upstream unreachable")),
        ],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5, 10]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn non_transient_round_failure_ends_the_run() {
    let (fx, flaky) = flaky_fixture(
        "retry-invalid",
        vec![text("unreached")],
        vec![Attempt::Stream(
            Vec::new(),
            ProviderError::HttpStatus {
                status: 400,
                message: "in-stream error (invalid_request_error): bad".into(),
                retry_after: None,
            },
        )],
    )
    .await;
    let error = flaky.turn(&fx, "hi").await.expect_err("a 400 ends the run");
    assert_eq!(
        error.to_string(),
        "http status 400: in-stream error (invalid_request_error): bad"
    );
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_round_can_succeed_on_the_ninth_attempt() {
    let failures = (1..=8)
        .map(|attempt| Attempt::Stream(Vec::new(), server_error(&format!("attempt {attempt}"))))
        .collect();
    let (fx, flaky) = flaky_fixture("retry-ninth-attempt", vec![text("done")], failures).await;
    assert_eq!(
        flaky
            .turn(&fx, "hi")
            .await
            .expect("the ninth attempt succeeds"),
        FinishReason::Stop
    );
    flaky.clock.assert_waits(&[5, 10, 20, 40, 60, 60, 60, 60]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 9);
}

#[tokio::test]
async fn a_round_returns_the_ninth_failure() {
    let failures = (1..=9)
        .map(|attempt| Attempt::Stream(Vec::new(), server_error(&format!("attempt {attempt}"))))
        .collect();
    let (fx, flaky) = flaky_fixture("retry-limit", vec![text("unreached")], failures).await;
    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("the ninth failure ends the run");
    assert_eq!(
        error.to_string(),
        "http status 500: in-stream error (server_error): attempt 9"
    );
    flaky.clock.assert_waits(&[5, 10, 20, 40, 60, 60, 60, 60]);
    let requests = flaky.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 9);
    assert!(
        requests
            .iter()
            .all(|request| format!("{request:?}") == format!("{:?}", requests[0]))
    );
}

#[tokio::test]
async fn a_stream_cut_after_partial_output_is_retried_without_it() {
    let (fx, flaky) = flaky_fixture(
        "retry-after-partial-output",
        vec![
            vec![
                FakeStep::Reasoning("plan the todo".into()),
                FakeStep::ToolCall {
                    name: "todo".into(),
                    input: json!({"ops": []}),
                },
            ],
            text("done"),
            text("checked"),
        ],
        vec![
            Attempt::Play,
            Attempt::Stream(
                vec![
                    FakeStep::Reasoning("half a thought".into()),
                    FakeStep::Text("partial answer".into()),
                ],
                body_break(),
            ),
        ],
    )
    .await;
    assert_eq!(
        flaky.turn(&fx, "plan, then answer").await.unwrap(),
        FinishReason::Stop
    );
    flaky.clock.assert_waits(&[5]);

    // The todo round, the cut attempt, its retry, and the round after the
    // completion check.
    let requests = flaky.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    assert_eq!(format!("{:?}", requests[1]), format!("{:?}", requests[2]));
    for request in &requests[2..] {
        let sent = format!("{request:?}");
        assert!(
            !sent.contains("half a thought") && !sent.contains("partial answer"),
            "{sent}"
        );
    }
    assert!(assistant_parts(&requests[3]).any(|part| matches!(
        part,
        Part::Reasoning { text, .. } if text == "plan the todo"
    )));

    let envelopes = fx.store.replay(fx.session).await.unwrap();
    let log: Vec<_> = envelopes.iter().map(|e| e.event.clone()).collect();
    assert!(log.iter().any(|event| matches!(
        event,
        Event::ReasoningReplace { text, .. } if text == "half a thought"
    )));
    assert!(log.iter().any(|event| matches!(
        event,
        Event::TextReplace { text, .. } if text == "partial answer"
    )));
    let views: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::StepStarted { step, view, .. } if [1, 2].contains(step) => view.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(views.len(), 2);
    assert_eq!(views[0], views[1], "the retry sends the cut attempt's view");

    let projection = fx.store.read_projection(fx.session).await.unwrap();
    let reply = projection
        .session
        .messages
        .iter()
        .rfind(|row| row.role == Role::Assistant)
        .unwrap();
    let answers: Vec<_> = reply
        .parts
        .iter()
        .filter_map(|part| match part {
            hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answers, ["done", "checked"]);

    // Each recorded cut, rebuilt from the log, reproduces the live request.
    let mut attempts = 0;
    for envelope in &envelopes {
        let Event::StepStarted { view, .. } = &envelope.event else {
            continue;
        };
        let recorded = view.as_ref().expect("step has a view");
        let end = envelopes.partition_point(|entry| entry.seq <= recorded.cut);
        let projection = Projection::from_events(&envelopes[..end]);
        let rebuilt = kernel::request::request(
            &View {
                projection: &projection,
            },
            &fx.system,
            fx.tools.iter().map(|tool| tool.schema()).collect(),
            &fx.model,
            &AgentName::new(kernel::AGENT),
            &AttachmentData::new(),
            None,
        );
        assert_eq!(
            kernel::request::request_sha256(&rebuilt).unwrap(),
            recorded.request_sha256
        );
        assert_eq!(format!("{rebuilt:?}"), format!("{:?}", requests[attempts]));
        attempts += 1;
    }
    assert_eq!(attempts, 4);
}

#[tokio::test]
async fn a_tool_call_of_a_cut_attempt_is_not_run() {
    let command = json!({"command": "echo ran >> count.txt"});
    // The fixture drops the last event: the round's end, after a complete call.
    let mut cut = call("bash", command.clone());
    cut.push(FakeStep::Finish(FinishReason::ToolCalls));
    let (fx, flaky) = flaky_fixture(
        "retry-after-tool-call",
        vec![call("bash", command), text("done"), text("checked")],
        vec![Attempt::Stream(cut, body_break())],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "count").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5]);
    assert_eq!(
        std::fs::read_to_string(fx.dir.join("count.txt")).unwrap(),
        "ran\n"
    );

    let log = fx.log().await;
    let calls: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::ToolCallRequested { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    let results: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult { call, .. } | Event::ToolError { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(results, [calls[1]], "only the retry's call ran");
    let requests = flaky.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    for request in &requests[2..] {
        let sent: Vec<_> = assistant_parts(request)
            .filter(|part| matches!(part, Part::Tool { .. }))
            .collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
    }
}

fn body_break() -> ProviderError {
    ProviderError::StreamInterrupted("Transport error: error decoding response body".into())
}

#[tokio::test]
async fn a_stream_break_after_the_final_frame_completes_the_round() {
    let (fx, flaky) = flaky_fixture(
        "break-after-final-frame",
        vec![text("unreached")],
        vec![Attempt::Break(
            vec![
                FakeStep::Text("the answer".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            body_break(),
        )],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    let log = fx.log().await;
    assert!(
        !log.iter()
            .any(|event| matches!(event, Event::PartDeleted { .. }))
    );
    assert!(log.iter().any(|event| matches!(
        event,
        Event::StepFinished {
            step: 0,
            finish: FinishReason::Stop,
            ..
        }
    )));
    let projection = fx.store.read_projection(fx.session).await.unwrap();
    let reply = projection.session.messages.last().unwrap();
    assert!(
        matches!(
            reply.parts.as_slice(),
            [hya_proto::PartProjection::Text { text, .. }] if text == "the answer"
        ),
        "{:?}",
        reply.parts
    );
}

#[tokio::test]
async fn a_stream_break_after_the_final_frame_runs_its_tool_call() {
    let mut round = call("bash", json!({"command": "echo ran >> count.txt"}));
    round.push(FakeStep::Finish(FinishReason::ToolCalls));
    let (fx, flaky) = flaky_fixture(
        "break-after-final-frame-tool",
        vec![text("done"), text("checked")],
        vec![Attempt::Break(round, body_break())],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "count").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[]);
    assert_eq!(
        std::fs::read_to_string(fx.dir.join("count.txt")).unwrap(),
        "ran\n"
    );
    // The tool round, the round after it, and the round after the completion check.
    assert_eq!(flaky.requests.lock().unwrap().len(), 3);
    assert!(
        !fx.log()
            .await
            .iter()
            .any(|event| matches!(event, Event::PartDeleted { .. }))
    );
}

#[tokio::test]
async fn a_cut_during_tool_input_is_retried_without_it() {
    // The fixture drops the last event, the call's request: only its input streamed.
    let (fx, flaky) = flaky_fixture(
        "retry-after-tool-input",
        vec![text("done")],
        vec![Attempt::Stream(
            call("bash", json!({"command": "echo ran >> count.txt"})),
            body_break(),
        )],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5]);
    assert!(!fx.dir.join("count.txt").exists());
    let log = fx.log().await;
    let started = log
        .iter()
        .find_map(|event| match event {
            Event::ToolInputStart { part, .. } => Some(*part),
            _ => None,
        })
        .unwrap();
    assert!(
        log.iter()
            .any(|event| matches!(event, Event::PartDeleted { part, .. } if *part == started))
    );
    let projection = fx.store.read_projection(fx.session).await.unwrap();
    let reply = projection.session.messages.last().unwrap();
    assert!(
        matches!(
            reply.parts.as_slice(),
            [hya_proto::PartProjection::Text { text, .. }] if text == "done"
        ),
        "{:?}",
        reply.parts
    );
}

#[tokio::test]
async fn a_cancel_during_the_wait_ends_the_run_with_the_parts_deleted() {
    let (fx, flaky) = flaky_fixture(
        "retry-cancelled-wait",
        vec![text("unreached")],
        vec![Attempt::Stream(
            vec![
                FakeStep::Reasoning("half a thought".into()),
                FakeStep::Text("partial answer".into()),
            ],
            body_break(),
        )],
    )
    .await;
    let cancel = CancellationToken::new();
    // The deletion is appended before the wait starts.
    let publish = |envelope: Envelope| {
        if matches!(envelope.event, Event::PartDeleted { .. }) {
            cancel.cancel();
        }
    };
    let error = fx
        .turn_with_publish("hi", &cancel, &publish)
        .await
        .expect_err("the cancelled wait ends the run");
    assert!(matches!(
        error,
        CoreError::Provider(error) if matches!(*error, ProviderError::StreamInterrupted(_))
    ));
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    let deleted = fx
        .log()
        .await
        .iter()
        .filter(|event| matches!(event, Event::PartDeleted { .. }))
        .count();
    assert_eq!(deleted, 2);
    let projection = fx.store.read_projection(fx.session).await.unwrap();
    assert!(projection.session.messages.last().unwrap().parts.is_empty());
}

#[tokio::test]
async fn a_round_returns_the_ninth_failure_after_partial_output() {
    let failures = (1..=9)
        .map(|attempt| {
            Attempt::Stream(
                vec![FakeStep::Reasoning(format!("thought {attempt}"))],
                server_error(&format!("attempt {attempt}")),
            )
        })
        .collect();
    let (fx, flaky) = flaky_fixture("retry-partial-limit", vec![text("unreached")], failures).await;
    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("the ninth failure ends the run");
    assert_eq!(
        error.to_string(),
        "http status 500: in-stream error (server_error): attempt 9"
    );
    flaky.clock.assert_waits(&[5, 10, 20, 40, 60, 60, 60, 60]);
    let requests = flaky.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 9);
    assert!(
        requests
            .iter()
            .all(|request| format!("{request:?}") == format!("{:?}", requests[0]))
    );
    // Every attempt stays in the log; only the last, unretried one stays in the message.
    let log = fx.log().await;
    let recorded = log
        .iter()
        .filter(|event| matches!(event, Event::ReasoningReplace { .. }))
        .count();
    assert_eq!(recorded, 9);
    let projection = fx.store.read_projection(fx.session).await.unwrap();
    let reply = projection.session.messages.last().unwrap();
    assert!(
        matches!(
            reply.parts.as_slice(),
            [hya_proto::PartProjection::Reasoning { text, .. }] if text == "thought 9"
        ),
        "{:?}",
        reply.parts
    );
}

#[tokio::test]
async fn a_completed_round_resets_the_retry_count() {
    let failures = (1..=18)
        .map(|attempt| {
            if attempt == 9 {
                Attempt::Play
            } else {
                Attempt::Stream(Vec::new(), server_error(&format!("attempt {attempt}")))
            }
        })
        .collect();
    let (fx, flaky) = flaky_fixture(
        "retry-next-round",
        vec![call("todo", json!({"ops": []}))],
        failures,
    )
    .await;
    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("the later round exhausts its own retries");
    assert_eq!(
        error.to_string(),
        "http status 500: in-stream error (server_error): attempt 18"
    );
    flaky
        .clock
        .assert_waits(&[5, 10, 20, 40, 60, 60, 60, 60, 5, 10, 20, 40, 60, 60, 60, 60]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 18);
}

#[test]
fn a_retry_wait_must_fit_before_final() {
    run_deadline_test("kernel::a_retry_wait_must_fit_before_final_child", "retry");
}

#[tokio::test]
async fn a_retry_wait_must_fit_before_final_child() {
    if std::env::var("HYA_BARE_DEADLINE_TEST").as_deref() != Ok("retry") {
        return;
    }
    let (fx, flaky) = flaky_fixture(
        "retry-deadline",
        vec![text("unreached")],
        vec![Attempt::Stream(
            Vec::new(),
            server_error("upstream unreachable"),
        )],
    )
    .await;
    deadline::install(Duration::from_secs(94));
    let deadline = deadline::current().unwrap();
    assert_eq!(deadline.stage(), Stage::Normal);
    assert!(deadline.remaining() < deadline.final_window() + Duration::from_secs(5));

    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("the first retry wait would reach Final");
    assert_eq!(
        error.to_string(),
        "http status 500: in-stream error (server_error): upstream unreachable"
    );
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    assert_eq!(deadline.stage(), Stage::Normal);
}

#[tokio::test]
async fn read_returns_text_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let text = "first line\n\"quoted\" and a \\ backslash\n";
    std::fs::write(dir.path().join("text.txt"), text).unwrap();
    let read = kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == "read")
        .unwrap();
    let ctx = tool_ctx(dir.path(), &CancellationToken::new());
    let result = read
        .execute(&ctx, json!({"path": "text.txt"}))
        .await
        .unwrap();

    assert_eq!(tool_result_text(&result), text);
    assert_eq!(result, json!({"output": text}));
}

fn encoded_test_image(format: image::ImageFormat) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 0, 0]));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, format).unwrap();
    encoded.into_inner()
}

async fn read_image_file(bytes: &[u8]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("image.data"), bytes).unwrap();
    let read = kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == "read")
        .unwrap();
    let ctx = tool_ctx(dir.path(), &CancellationToken::new());
    read.execute(&ctx, json!({"path": "image.data"}))
        .await
        .unwrap()
}

#[tokio::test]
async fn read_corrupt_png_reports_decode_error_without_attachment() {
    let mut png = encoded_test_image(image::ImageFormat::Png);
    assert_eq!(&png[37..41], b"IDAT");
    png[41] = 0; // Corrupt the zlib header after the intact IHDR.
    let result = read_image_file(&png).await;
    assert!(
        result.get("attachments").is_none(),
        "corrupt PNG was attached"
    );
    let notice = tool_result_text(&result);
    assert!(notice.starts_with(&format!(
        "Image file (image/png, {} bytes). It does not decode as image/png (",
        png.len()
    )));
    assert!(notice.contains("), so it is not attached."));
}

#[tokio::test]
async fn read_truncated_jpeg_reports_decode_error_without_attachment() {
    let mut jpeg = encoded_test_image(image::ImageFormat::Jpeg);
    jpeg.truncate(32);
    let result = read_image_file(&jpeg).await;
    assert!(
        result.get("attachments").is_none(),
        "truncated JPEG was attached"
    );
    let notice = tool_result_text(&result);
    assert!(
        notice.starts_with("Image file (image/jpeg, 32 bytes). It does not decode as image/jpeg (")
    );
    assert!(notice.contains("), so it is not attached."));
}

#[tokio::test]
async fn read_valid_images_attach_original_bytes() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use image::ImageFormat;

    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::Gif, "image/gif"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let bytes = encoded_test_image(format);
        let result = read_image_file(&bytes).await;
        assert_eq!(
            result,
            json!({
                "output": format!("Image file ({mime}, {} bytes).", bytes.len()),
                "attachments": [{
                    "type": "file",
                    "mime": mime,
                    "url": format!("data:{mime};base64,{}", STANDARD.encode(&bytes)),
                }],
            }),
        );
    }
}

/// The image goes in the request after the read and in no later request, also
/// after a round of only tool calls; a request rebuilt from the log for the
/// same step is the one sent.
#[tokio::test]
async fn read_image_reaches_the_model_once_from_live_and_replayed_history() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let png = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=";
    let mut looked = text("A board.");
    looked.extend(call("todo", json!({"ops": []})));
    let fx = fixture(
        "read-image",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "board.data"})),
            call("todo", json!({"ops": []})),
            looked,
            text("done"),
        ]),
    )
    .await;
    std::fs::write(fx.dir.join("board.data"), STANDARD.decode(png).unwrap()).unwrap();
    assert_eq!(
        fx.turn("look at the board", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );

    let requests = fx.provider.requests();
    assert!(requests.len() >= 4, "{} requests", requests.len());
    let wire: Vec<serde_json::Value> = requests
        .iter()
        .map(|request| {
            AnthropicMessagesProtocol
                .encode_for_model(request, None, true)
                .unwrap()
        })
        .collect();
    let result = &wire[1]["messages"][2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["is_error"], false);
    assert_eq!(
        result["content"][0]["text"],
        "Image file (image/png, 80 bytes)."
    );
    assert_eq!(
        result["content"][1],
        json!({"type": "image", "source": {
            "type": "base64", "media_type": "image/png", "data": png,
        }})
    );
    for later in &wire[2..] {
        let later = later.to_string();
        assert!(!later.contains(png), "{later}");
        assert!(
            later.contains(
                "Image file (image/png, 80 bytes).\\n\\n[An image is sent to you only \
                 in the request right after the call that produced it; later requests, \
                 like this one, carry this note instead. This is the normal course, not \
                 a failure. To look at the image of this result (image/png), make the \
                 call again"
            ),
            "{later}"
        );
    }

    let envelopes = fx.store.replay(fx.session).await.unwrap();
    for (step, sent) in wire.iter().enumerate().skip(1) {
        let started = envelopes
            .iter()
            .position(
                |entry| matches!(entry.event, Event::StepStarted { step: s, .. } if s as usize == step),
            )
            .unwrap();
        let projection = Projection::from_events(&envelopes[..started]);
        let rebuilt = kernel::request::request(
            &View {
                projection: &projection,
            },
            &fx.system,
            fx.tools.iter().map(|tool| tool.schema()).collect(),
            &fx.model,
            &AgentName::new(kernel::AGENT),
            &AttachmentData::new(),
            None,
        );
        assert_eq!(
            AnthropicMessagesProtocol
                .encode_for_model(&rebuilt, None, true)
                .unwrap(),
            *sent,
            "step {step}"
        );
    }
}

#[tokio::test]
async fn read_oversized_image_returns_resize_guidance_without_attachment() {
    let fx = fixture(
        "read-oversized-image",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "board.png"})),
            text("done"),
        ]),
    )
    .await;
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    // One byte above the PNG file limit for the 2 MiB serialized attachment.
    png.resize(1_572_814, 0);
    std::fs::write(fx.dir.join("board.png"), png).unwrap();

    assert_eq!(
        fx.turn("look at the board", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );

    let requests = fx.provider.requests();
    let output = assistant_parts(&requests[1])
        .find_map(|part| match part {
            Part::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "read" => Some(output),
            _ => None,
        })
        .unwrap();
    assert!(output.get("attachments").is_none(), "image was attached");
    let notice = output["output"].as_str().unwrap();
    assert!(notice.contains("Image file (image/png, 1572814 bytes)."));
    assert!(notice.contains("larger than the limit of 1572813 bytes"));
    assert!(notice.contains("not attached"));
    assert!(notice.contains("Downscale or crop it with a program and read the smaller file"));
}

#[tokio::test]
async fn large_image_result_stays_below_rewrite_pressure() {
    let fx = fixture(
        "large-image-pressure",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "board.png"})),
            text("done"),
            text("checked"),
        ])
        .with_max_context(8_000)
        .with_tool_images(),
    )
    .await;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};

    let mut png = Vec::new();
    PngEncoder::new_with_quality(
        &mut png,
        CompressionType::Uncompressed,
        FilterType::NoFilter,
    )
    .write_image(&[0; 384 * 1024], 512, 256, ExtendedColorType::Rgb8)
    .unwrap();
    std::fs::write(fx.dir.join("board.png"), png).unwrap();

    assert_eq!(
        fx.turn("look at the board", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 3);
    // The fixed image charge crosses the checkpoint threshold, while an omission notice would not.
    assert_eq!(
        [
            goal_checkpoints(&requests[0]),
            goal_checkpoints(&requests[1])
        ],
        [0, 1]
    );
    assert!(assistant_parts(&requests[1]).any(|part| matches!(
        part,
        Part::Tool { state: ToolPartState::Completed { output, .. }, .. }
        if output["attachments"][0]["url"].as_str().is_some_and(|url| url.len() > 500_000)
    )));
    assert!(compactions(&fx.log().await).is_empty());
}

#[tokio::test]
async fn a3_cancel_is_an_observation() {
    let fx = fixture(
        "a3",
        FakeProvider::scripted_turns(vec![call(
            "bash",
            json!({"command": "sleep 30 & echo $! > pid; wait"}),
        )]),
    )
    .await;
    let cancel = CancellationToken::new();
    let pid_file = fx.dir.join("pid");
    let _sleep = PidFile(pid_file.clone());
    let canceller = tokio::spawn({
        let cancel = cancel.clone();
        let pid_file = pid_file.clone();
        async move {
            while !std::fs::read_to_string(&pid_file).is_ok_and(|pid| pid.ends_with('\n')) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        }
    });
    let finish = tokio::time::timeout(Duration::from_secs(20), fx.turn("sleep", &cancel))
        .await
        .expect("cancel ends the run");
    canceller.await.unwrap();
    assert_eq!(finish.unwrap(), FinishReason::Cancelled);
    let log = fx.log().await;
    assert_paired(&log);
    assert_eq!(tool_errors(&log), vec!["cancelled"]);
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let pid = pid.trim();
    let mut alive = true;
    for _ in 0..200 {
        let stat = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&stat.stdout);
        if stat.trim().is_empty() || stat.trim().starts_with('Z') {
            alive = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!alive, "the command's child process {pid} is still running");
}

/// Kills the process whose PID a test command wrote to this file, also when an assertion fails.
struct PidFile(PathBuf);

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0)
            && let Ok(pid) = pid.trim().parse::<libc::pid_t>()
            && pid > 0
        {
            // SAFETY: this positive PID belongs to the test's command.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

const BASH_YIELD_COMMAND: &str =
    "echo $$ > pgid; echo first; while [ ! -e go ]; do sleep 0.01; done; echo second; exit 7";

/// Lets the command finish, kills its group and removes the call's
/// directory, also when an assertion fails.
struct LeftRunning {
    go: PathBuf,
    pgid: Option<libc::pid_t>,
    dir: Option<PathBuf>,
}

impl LeftRunning {
    fn new(workdir: &Path) -> Self {
        Self {
            go: workdir.join("go"),
            pgid: None,
            dir: None,
        }
    }
}

impl Drop for LeftRunning {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.go, "");
        if let Some(pgid) = self.pgid.filter(|pgid| *pgid > 0) {
            // SAFETY: this positive group ID belongs to the test's command.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A command still running when the call's `timeout` ends is not signalled:
/// the call answers with the output so far and where the rest goes.
#[tokio::test]
async fn bash_timeout_leaves_the_command_running_in_the_background() {
    let fx = fixture(
        "bash-background",
        FakeProvider::scripted_turns(vec![
            call(
                "bash",
                json!({
                    "command": BASH_YIELD_COMMAND,
                    "timeout": 1,
                }),
            ),
            text("ok"),
        ]),
    )
    .await;
    let mut left = LeftRunning::new(&fx.dir);
    let finish = tokio::time::timeout(
        Duration::from_secs(15),
        fx.turn("run", &CancellationToken::new()),
    )
    .await
    .expect("the call returned while the command kept running")
    .unwrap();
    let log = fx.log().await;
    let result = log
        .iter()
        .find_map(|event| match event {
            Event::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .unwrap();
    assert_bash_yield(result, &mut left, "1s (the `timeout` this call passed)").await;
    assert_eq!(finish, FinishReason::Stop);
    assert_paired(&log);
}

async fn assert_bash_yield(
    result: &serde_json::Value,
    left: &mut LeftRunning,
    expected_wait: &str,
) {
    let note = result["still_running"].as_str().unwrap_or_default();
    left.pgid = note
        .split_once("process group ")
        .and_then(|(_, rest)| rest.split_once(';'))
        .and_then(|(pgid, _)| pgid.parse().ok());
    left.dir = note
        .split_once("goes to '")
        .and_then(|(_, rest)| rest.split_once("/output'"))
        .map(|(dir, _)| PathBuf::from(dir));

    assert_eq!(result["exit_code"], serde_json::Value::Null);
    assert!(result.get("timed_out").is_none(), "{result}");
    // The shell may not have printed yet; it prints "second" only after the signal file exists.
    let model_text = tool_result_text(result);
    let status = format!("[still running: {note}]");
    assert_eq!(model_text.lines().last(), Some(status.as_str()));
    let output = model_text.strip_suffix(&status).unwrap();
    assert!("first\n".starts_with(output), "{output:?}");
    assert!(note.contains(&format!("waited {expected_wait}")), "{note}");
    assert!(note.contains("still running then"), "{note}");
    assert!(note.contains("was not stopped"), "{note}");
    let pgid = left
        .pgid
        .unwrap_or_else(|| panic!("no process group in {note}"));
    let dir = left
        .dir
        .clone()
        .unwrap_or_else(|| panic!("no output file in {note}"));
    let (out, exit) = (dir.join("output"), dir.join("exit_code"));
    assert!(
        note.contains(&format!("`tail -n 20 '{}'`", out.display())),
        "{note}"
    );
    assert!(
        note.contains(&format!("`cat '{}'`", exit.display())),
        "{note}"
    );
    assert!(
        note.contains("To wait again, sleep briefly in another call"),
        "{note}"
    );
    assert!(note.contains(&format!("`kill -- -{pgid}`")), "{note}");
    // SAFETY: signal 0 only checks that the group exists.
    assert_eq!(
        unsafe { libc::killpg(pgid, 0) },
        0,
        "the command was stopped"
    );

    std::fs::write(&left.go, "").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !exit.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the exit code of the background command was written");
    // The group has ended, so its ID must not be signalled again.
    left.pgid = None;
    assert_eq!(std::fs::read_to_string(&exit).unwrap(), "7\n");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "first\nsecond\n");
    assert_eq!(
        std::fs::read_to_string(left.go.with_file_name("pgid"))
            .unwrap()
            .trim(),
        pgid.to_string()
    );
}

/// A job the command starts with plain `&` keeps running after the command
/// ended, and the call returns at once.
#[tokio::test]
async fn bash_leaves_a_background_job_running_after_the_command_ends() {
    let fx = fixture(
        "bash-job",
        FakeProvider::scripted_turns(vec![
            call(
                "bash",
                json!({"command": "(while [ ! -e go ]; do sleep 0.01; done; echo done > marker) & echo $! > job; echo started; exit 3"}),
            ),
            call("bash", json!({"command": "true", "timeout": 0})),
            text("ok"),
        ]),
    )
    .await;
    let job = PidFile(fx.dir.join("job"));
    let finish = tokio::time::timeout(
        Duration::from_secs(15),
        fx.turn("run", &CancellationToken::new()),
    )
    .await
    .expect("the call returned while the job kept running")
    .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let log = fx.log().await;
    assert_paired(&log);
    let results: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["exit_code"], 3);
    assert_eq!(tool_result_text(results[0]), "started\n[exit code: 3]");
    assert_eq!(tool_errors(&log).len(), 1, "timeout 0 is an input error");
    let pid = std::fs::read_to_string(&job.0)
        .unwrap()
        .trim()
        .parse::<libc::pid_t>()
        .unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "the job was killed");

    std::fs::write(fx.dir.join("go"), "").unwrap();
    let marker = fx.dir.join("marker");
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::fs::read_to_string(&marker).unwrap_or_default() != "done\n" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the job finished its work");
}

/// Runs `child` in a test process with its own temporary directory, then
/// checks that its `bash` calls removed every directory they created.
fn run_bash_child(child: &str, case: &str) {
    let tmp = std::env::temp_dir().join(format!("hya-kernel-bash-{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", child, "--nocapture"])
        .env("HYA_BASH_TEST", case)
        .env("TMPDIR", &tmp)
        .output()
        .unwrap();
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("hya-bash-"))
        })
        .collect();
    std::fs::remove_dir_all(&tmp).unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        left.is_empty(),
        "{case}: call directories left behind: {left:?}"
    );
}

#[test]
fn bash_waits_at_most_a_tenth_of_the_run_budget() {
    for case in ["short", "900", "1800", "3600-default", "3600-explicit"] {
        run_bash_child("kernel::bash_budget_share_child", case);
    }
}

#[tokio::test]
async fn bash_budget_share_child() {
    let (budget, requested, waited) = match std::env::var("HYA_BASH_TEST").as_deref() {
        Ok("short") => (20, Some(3), "2s"),
        Ok("900") => (900, None, "90s"),
        Ok("1800") => (1800, Some(600), "180s"),
        Ok("3600-default") => (3600, None, "300s"),
        Ok("3600-explicit") => (3600, Some(600), "360s"),
        _ => return,
    };
    let workdir = std::env::temp_dir();
    let mut left = LeftRunning::new(&workdir);
    let bash = kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == "bash")
        .unwrap();
    let ctx = tool_ctx(&workdir, &CancellationToken::new());
    let mut input = json!({"command": BASH_YIELD_COMMAND});
    if let Some(seconds) = requested {
        input["timeout"] = json!(seconds);
    }
    let reason = if waited == "300s" {
        "the default; pass `timeout` in seconds to request a longer wait"
    } else {
        "the run's time budget lets one call wait at most a tenth of it"
    };
    deadline::install(Duration::from_secs(budget));
    // Use the real timer in the short case. For the long examples, Tokio can
    // advance the wait while the shell is held open by the signal file.
    if budget != 20 {
        tokio::time::pause();
    }
    let result = bash.execute(&ctx, input).await.unwrap();
    if budget != 20 {
        tokio::time::resume();
    }
    assert_bash_yield(&result, &mut left, &format!("{waited} ({reason})")).await;
}

#[cfg(target_os = "linux")]
#[test]
fn bash_waits_for_worker_threads_and_fork_handoffs() {
    for case in ["full", "bare"] {
        run_bash_child("kernel::bash_group_lifetime_child", case);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn bash_group_lifetime_child() {
    let Ok(case) = std::env::var("HYA_BASH_TEST") else {
        return;
    };
    let dir = std::env::temp_dir();
    let build = std::process::Command::new("cc")
        .args(["-pthread", "-o", "worker"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/worker_thread.c"
        ))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    deadline::install(Duration::from_secs(180));
    let bash = match case.as_str() {
        "full" => hya_tool::ToolRegistry::from_tool_families(&["hya/base-tools"])
            .get("bash")
            .unwrap(),
        "bare" => kernel::tools::builtin()
            .into_iter()
            .find(|tool| tool.name() == "bash")
            .unwrap(),
        _ => return,
    };
    let pid_file = dir.join("worker-ids");
    for cancel in [false, true] {
        let token = CancellationToken::new();
        let mut ctx = tool_ctx(&dir, &token);
        ctx.permission = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Bash,
            "*",
            Mode::Allow,
        )]))
        .0;
        // Both intermediate shells exit as soon as they have forked their child.
        let call = bash.execute(
            &ctx,
            json!({"command": "( (./worker & exit) & exit) & exit 7"}),
        );
        let observe = async {
            let (pid, tid) = loop {
                if let Ok(ids) = std::fs::read_to_string(&pid_file)
                    && let Some((pid, tid)) = ids.split_once(' ')
                    && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .is_ok_and(|s| s.contains(") Z "))
                {
                    break (pid.to_owned(), tid.to_owned());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            let worker = PathBuf::from(format!("/proc/{pid}/task/{tid}"));
            assert!(worker.exists());
            if cancel {
                token.cancel();
            } else {
                std::fs::write(dir.join("go"), "").unwrap();
            }
            worker
        };
        let (result, worker) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(call, observe)
        })
        .await
        .expect("group completion");
        assert!(!worker.exists(), "worker still present at completion");
        if cancel {
            assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
        } else {
            let result = result.unwrap();
            assert!(
                result["output"]
                    .as_str()
                    .unwrap()
                    .contains("worker finished"),
                "{result}"
            );
            let exit = if case == "full" {
                &result["metadata"]["exit"]
            } else {
                &result["exit_code"]
            };
            assert_eq!(exit, 7);
            std::fs::remove_file(dir.join("go")).unwrap();
        }
        std::fs::remove_file(&pid_file).unwrap();
    }
}

/// A wait the run's time budget cut short still kills the command's group,
/// including a job it started: the run is ending and nothing could follow
/// the command up.
#[test]
fn bash_kills_a_command_cut_off_by_the_run_budget() {
    run_bash_child("kernel::bash_budget_cap_child", "budget-cap");
}

#[tokio::test]
async fn bash_budget_cap_child() {
    if std::env::var_os("HYA_BASH_TEST").is_none() {
        return;
    }
    let fx = fixture(
        "bash-budget-cap",
        FakeProvider::scripted_turns(vec![
            call(
                "bash",
                json!({"command": "sleep 30 & echo $! > pid; echo started; wait"}),
            ),
            text("done"),
        ]),
    )
    .await;
    let job = PidFile(fx.dir.join("pid"));
    // With less than a tenth of the budget left, the end-of-budget cap
    // wins even though the shared cap has a five-second floor.
    deadline::install(Duration::from_secs(5));
    let deadline = deadline::current().unwrap();
    tokio::time::sleep(
        deadline
            .remaining()
            .saturating_sub(Duration::from_millis(400)),
    )
    .await;
    let finish = tokio::time::timeout(
        Duration::from_secs(15),
        fx.turn("run", &CancellationToken::new()),
    )
    .await
    .expect("the capped call returned")
    .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let log = fx.log().await;
    assert_paired(&log);
    let result = log
        .iter()
        .find_map(|event| match event {
            Event::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .unwrap();
    assert_eq!(result["exit_code"], serde_json::Value::Null);
    assert!(result.get("still_running").is_none(), "{result}");
    let note = result["timed_out"].as_str().unwrap();
    let model_text = tool_result_text(result);
    let status = format!("[stopped: {note}]");
    assert_eq!(model_text.lines().last(), Some(status.as_str()));
    let output = model_text.strip_suffix(&status).unwrap();
    assert!("started\n".starts_with(output), "{output:?}");

    assert!(
        note.contains("the run's time budget capped the timeout")
            && note.contains("killed its process group"),
        "{note}"
    );

    let bash = kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == "bash")
        .unwrap();
    let ctx = tool_ctx(&fx.dir, &CancellationToken::new());
    let result = bash
        .execute(&ctx, json!({"command": "touch unexpected"}))
        .await
        .unwrap();
    assert!(!fx.dir.join("unexpected").exists());
    assert_eq!(result["exit_code"], serde_json::Value::Null);
    let note = result["timed_out"].as_str().unwrap();
    assert_eq!(tool_result_text(&result), format!("[not started: {note}]"));

    // Without the file the shell was killed before it could name the job,
    // which was then in the killed group too.
    let Ok(pid) = std::fs::read_to_string(&job.0) else {
        return;
    };
    let pid = pid.trim();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let stat = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&stat.stdout);
            if stat.trim().is_empty() || stat.trim().starts_with('Z') {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the capped command's job {pid} is still running"));
}

#[tokio::test]
async fn bash_uses_bash_syntax_when_available() {
    let bash_available = std::process::Command::new("bash")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let fx = fixture(
        "bash-syntax",
        FakeProvider::scripted_turns(vec![
            call(
                "bash",
                json!({"command": r#"
                    printf 'shell\n'
                    if [ -n "${BASH_VERSION-}" ]; then
                        eval '[[ word == w* ]] && printf "bash\n"'
                    fi
                    false | true
                "#}),
            ),
            text("ok"),
        ]),
    )
    .await;
    assert_eq!(
        fx.turn("run", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let log = fx.log().await;
    let result = log
        .iter()
        .find_map(|event| match event {
            Event::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .unwrap();
    assert_eq!(result["exit_code"], 0);
    assert!(result["output"].as_str().unwrap().starts_with("shell\n"));
    if bash_available {
        assert_eq!(result["output"], "shell\nbash\n[exit code: 0]");
    }
}

#[tokio::test]
async fn bash_reports_exit_codes_without_command_output() {
    let bash = kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == "bash")
        .unwrap();
    let ctx = tool_ctx(Path::new("."), &CancellationToken::new());
    for (command, code) in [("true", 0), ("kill -TERM $$", 143)] {
        let result = bash
            .execute(&ctx, json!({"command": command}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], code);
        assert_eq!(tool_result_text(&result), format!("[exit code: {code}]"));
    }
}

#[tokio::test]
async fn bash_sets_pythonunbuffered() {
    let bash = kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == "bash")
        .unwrap();
    let ctx = tool_ctx(Path::new("."), &CancellationToken::new());
    let result = bash
        .execute(
            &ctx,
            json!({"command": r#"printf '%s' "$PYTHONUNBUFFERED""#}),
        )
        .await
        .unwrap();
    assert_eq!(result["output"], "1\n[exit code: 0]");
}

#[test]
fn bash_keeps_large_output_on_normal_exit() {
    run_bash_child("kernel::bash_output_child", "large-output");
}

#[test]
fn bash_returns_all_output_at_the_limit() {
    run_bash_child("kernel::bash_output_child", "output-limit");
}

#[test]
fn bash_yield_names_the_same_file_in_the_output_marker() {
    run_bash_child("kernel::bash_output_child", "large-yield");
}

#[tokio::test]
async fn bash_output_child() {
    let (size, yields) = match std::env::var("HYA_BASH_TEST").as_deref() {
        Ok("large-output") => (50_000, false),
        Ok("output-limit") => (30_000, false),
        Ok("large-yield") => (50_000, true),
        _ => return,
    };
    let workdir = std::env::temp_dir();
    let mut left = LeftRunning::new(&workdir);
    let bash = kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == "bash")
        .unwrap();
    let ctx = tool_ctx(&workdir, &CancellationToken::new());

    let padding = size - "first\nlast\n".len();
    let mut command = format!(
        r#"printf 'first\n'; head -c {padding} /dev/zero | tr '\000' x; printf 'last\n' >&2"#
    );
    if yields {
        command.push_str("; echo $$ > ready; while [ ! -e go ]; do sleep 0.01; done");
    }
    let call = bash.execute(&ctx, json!({"command": command, "timeout": 300}));
    tokio::pin!(call);
    let result = if yields {
        let ready = workdir.join("ready");
        tokio::select! {
            result = &mut call => panic!("Bash returned before the ready signal: {result:?}"),
            () = async {
                tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        if let Ok(pid) = std::fs::read_to_string(&ready)
                            && let Ok(pid) = pid.trim().parse()
                        {
                            left.pgid = Some(pid);
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("the command printed its output and signalled readiness");
            } => {}
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(300)).await;
        call.await.unwrap()
    } else {
        tokio::time::timeout(Duration::from_secs(15), call)
            .await
            .expect("the command ended")
            .unwrap()
    };

    let model_text = tool_result_text(&result);
    let status = if yields {
        format!(
            "[still running: {}]",
            result["still_running"].as_str().unwrap()
        )
    } else {
        "[exit code: 0]".to_string()
    };
    let output = model_text.strip_suffix(&status).unwrap();
    let expected = format!("first\n{}last\n", "x".repeat(padding));
    if size == 30_000 {
        assert_eq!(result["exit_code"], 0);
        assert_eq!(output, expected);
        // The parent test checks that the call's directory was removed.
        return;
    }

    // The parent owns the isolated temp root, including on an assertion failure.
    let marker = output
        .lines()
        .find(|line| line.starts_with("[… "))
        .expect("large output has an omission marker");
    let (_, rest) = marker.split_once(" bytes are in '").unwrap();
    let (path, _) = rest.split_once("'.").unwrap();
    let path = PathBuf::from(path);
    left.dir = path.parent().map(Path::to_path_buf);

    assert!(marker.starts_with("[… 20000 bytes omitted here. All 50000 bytes"));
    assert_eq!(output.len(), 30_000 + marker.len() + 2);
    assert!(output.starts_with(&expected[..10_000]));
    assert!(output.ends_with(&expected[size - 20_000..]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    if yields {
        assert_eq!(result["exit_code"], serde_json::Value::Null);
        let note = result["still_running"].as_str().unwrap();
        assert!(note.contains(&format!("goes to '{}'.", path.display())));
    } else {
        assert_eq!(result["exit_code"], 0);
    }
}

#[tokio::test]
async fn a4_model_rewrite() {
    let fx = fixture(
        "a4",
        FakeProvider::scripted_turns(vec![
            text("hi"),
            call("bash", json!({"command": "echo one"})),
            call("bash", json!({"command": "echo two"})),
            call("rewrite", json!({"summary": "ran two echoes"})),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    let cancel = CancellationToken::new();
    fx.turn("first task: greet", &cancel).await.unwrap();
    assert_eq!(
        fx.turn("second task: echo", &cancel).await.unwrap(),
        FinishReason::Stop
    );
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 6);
    let next = &requests[4];

    // No dangling call or result; the removed steps are gone.
    for part in assistant_parts(next) {
        if let Part::Tool { state, name, .. } = part {
            assert!(
                matches!(
                    state,
                    ToolPartState::Completed { .. } | ToolPartState::Error { .. }
                ),
                "{name} has no result"
            );
        }
    }
    let tools: Vec<_> = assistant_parts(next)
        .filter_map(|part| match part {
            Part::Tool { name, state, .. } => Some((name.as_str(), state)),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 1, "only the rewrite step remains: {tools:?}");

    // The marker repeats the removed user message verbatim.
    let marker = next
        .messages
        .iter()
        .find_map(|message| match message {
            Message::System { content, .. } if content.starts_with(COMPACT_CONTEXT_MARKER) => {
                Some(content)
            }
            _ => None,
        })
        .expect("marker message");
    assert!(marker.contains("first task: greet"), "{marker}");
    assert!(marker.contains("removed by the model"), "{marker}");
    assert!(next.messages.iter().any(|message| matches!(
        message,
        Message::User { parts, .. }
            if matches!(parts.as_slice(), [Part::Text { text, .. }] if text == "second task: echo")
    )));

    // The rewrite call and its summary stay, with the fixed result.
    let (name, state) = tools[0];
    assert_eq!(name, "rewrite");
    let ToolPartState::Completed { input, output, .. } = state else {
        panic!("rewrite did not complete: {state:?}");
    };
    assert_eq!(input, &json!({"summary": "ran two echoes"}));
    assert_eq!(
        output["note"],
        "The summary in this call is your own text, not an observation."
    );
    assert_eq!(output["removed_messages"], 3);

    // The removed events are still in the log.
    let log = fx.log().await;
    assert_paired(&log);
    let echoes = log
        .iter()
        .filter(|event| {
            matches!(event, Event::ToolCallRequested { name, .. } if name.as_str() == "bash")
        })
        .count();
    assert_eq!(echoes, 2);
    assert!(log.iter().any(|event| matches!(
        event,
        Event::ToolResult { output, .. } if output["output"] == "one\n[exit code: 0]"
    )));
    assert!(
        log.iter()
            .any(|event| matches!(event, Event::TextReplace { text, .. } if text == "hi"))
    );
    assert!(log.iter().any(|event| matches!(
        event,
        Event::ContextCompacted {
            strategy: CompactionStrategy::Rewrite,
            step_cut: Some(_),
            ..
        }
    )));
}

#[tokio::test]
async fn a5_rewrite_with_nothing_to_remove() {
    let fx = fixture(
        "a5",
        FakeProvider::scripted_turns(vec![
            call("rewrite", json!({"summary": "nothing yet"})),
            text("ok"),
        ]),
    )
    .await;
    let finish = fx.turn("start", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    let log = fx.log().await;
    assert_paired(&log);
    assert_eq!(tool_errors(&log), vec!["nothing to remove"]);
    assert!(
        !log.iter()
            .any(|event| matches!(event, Event::ContextCompacted { .. }))
    );
}

fn round_at(tokens: u64) -> Vec<FakeStep> {
    vec![
        FakeStep::ToolCall {
            name: "bash".to_string(),
            input: json!({"command": "echo hi"}),
        },
        FakeStep::Usage(TokenUsage {
            input: tokens,
            ..TokenUsage::default()
        }),
        FakeStep::Finish(FinishReason::ToolCalls),
    ]
}

fn goal_checkpoints(request: &CompletionRequest) -> usize {
    request
        .messages
        .iter()
        .filter(|message| {
            matches!(message, Message::System { content, .. } if content.starts_with("[hya] Goal checkpoint:"))
        })
        .count()
}

#[tokio::test]
async fn goal_checkpoint_is_asked_once_before_the_rewrite_threshold() {
    let fx = fixture(
        "goal-checkpoint",
        FakeProvider::scripted_turns(vec![
            round_at(2_000),
            round_at(13_000),
            round_at(14_000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    let finish = fx.turn("go", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);

    let requests = fx.provider.requests();
    let asked: Vec<usize> = requests.iter().map(goal_checkpoints).collect();
    // Below 60%: none; at 65%: asked; at 70%: already answered.
    assert_eq!(asked, [0, 0, 1, 0, 0]);
    let log = fx.log().await;
    assert!(
        !log.iter()
            .any(|event| matches!(event, Event::ContextCompacted { .. }))
    );
    assert_paired(&log);
}

#[tokio::test]
async fn goal_checkpoint_stays_out_after_rewrite() {
    let fx = fixture(
        "goal-checkpoint-rewrite",
        FakeProvider::scripted_turns(vec![
            round_at(2_000),
            round_at(13_000),
            round_at(17_000),
            round_at(14_000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    let finish = fx.turn("go", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);

    let requests = fx.provider.requests();
    let asked: Vec<usize> = requests.iter().map(goal_checkpoints).collect();
    assert_eq!(asked, [0, 0, 1, 0, 0, 0]);
    assert!(requests[3].messages.iter().any(|message| matches!(
        message,
        Message::System { content, .. } if content.starts_with("HYA_COMPACTED_CONTEXT")
    )));
    let log = fx.log().await;
    assert_eq!(
        log.iter()
            .filter(|event| matches!(
                event,
                Event::TextDelta { delta, .. } if delta.starts_with("[hya] Goal checkpoint:")
            ))
            .count(),
        1
    );
    assert_paired(&log);
}

/// A reduction extension for [`kernel::before_round`]: records the pressure it
/// is handed and, over the threshold, either folds every message before the
/// turn's user message behind a small marker (`fold`) or appends a note,
/// which changes the log without reducing the request.
struct Probe {
    fold: bool,
    seen: std::sync::Mutex<Vec<Option<Pressure>>>,
}

#[async_trait::async_trait]
impl Rewriter for Probe {
    async fn before_round(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        self.seen.lock().unwrap().push(cx.pressure);
        if !cx.pressure.is_some_and(Pressure::over) {
            return Ok(false);
        }
        let session = cx.session;
        let messages = &cx.projection.session.messages;
        let head = messages
            .iter()
            .rposition(|message| message.role == Role::User)
            .unwrap();
        let marker = MessageId::new();
        let part = PartId::new();
        let mut events = vec![
            Event::MessageStarted {
                session,
                message: marker,
                role: Role::System,
                agent: None,
                model: None,
            },
            Event::TextStart {
                session,
                message: marker,
                part,
            },
            Event::TextDelta {
                session,
                message: marker,
                part,
                // Only a folding probe writes a marker, which cuts the request.
                delta: if self.fold {
                    format!("{COMPACT_CONTEXT_MARKER}\nfolded by the probe")
                } else {
                    "a note from the probe".to_string()
                },
            },
            Event::TextEnd {
                session,
                message: marker,
                part,
            },
            Event::MessageFinished {
                session,
                message: marker,
                role: Role::System,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        if self.fold {
            events.push(Event::ContextCompacted {
                session,
                message: marker,
                strategy: CompactionStrategy::Native,
                from_message: messages[0].id,
                to_message: messages[head - 1].id,
                folded_count: u32::try_from(head).unwrap(),
                input_tokens_est: 0,
                threshold: 0,
                step_cut: None,
            });
        }
        cx.log.append(session, events).await?;
        Ok(true)
    }

    async fn rewrite_now(&self, _cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        Ok(false)
    }
}

struct StoreLog<'a>(&'a SessionStore);

#[async_trait::async_trait]
impl SessionLog for StoreLog<'_> {
    async fn projection(&self, session: SessionId) -> Result<Arc<Projection>, ToolError> {
        Ok(self.0.read_projection_shared(session).await.unwrap())
    }

    async fn append(&self, session: SessionId, events: Vec<Event>) -> Result<(), ToolError> {
        self.0.append_events(session, events).await.unwrap();
        Ok(())
    }
}

struct KernelTranscript(ModelRef);

#[async_trait::async_trait]
impl Transcript for KernelTranscript {
    async fn messages(&self, projection: &Projection) -> Result<Vec<Message>, ToolError> {
        Ok(kernel::request::projection_to_messages(
            &AgentName::new(kernel::AGENT),
            projection,
            &self.0,
            &AttachmentData::new(),
        ))
    }
}

/// A finished ~10k-token turn, then turn 2's user message and its started
/// assistant message; `kernel::before_round` on that message with a 10k
/// window (threshold 8,000) and a [`Probe`] as the only extension. Returns
/// whether the log changed, the log, and the pressures the probe saw.
async fn reduce_with(name: &str, fold: bool) -> (bool, Vec<Event>, Vec<Option<Pressure>>) {
    let fx = fixture(
        name,
        FakeProvider::scripted_turns(vec![text(&"earlier detail ".repeat(5_000))]),
    )
    .await;
    fx.turn("first", &CancellationToken::new()).await.unwrap();
    let session = fx.session;
    let user = MessageId::new();
    let part = PartId::new();
    let message = MessageId::new();
    fx.store
        .append_events(
            session,
            vec![
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
                    delta: "second".to_string(),
                },
                Event::TextEnd {
                    session,
                    message: user,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message: user,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
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
    let projection = fx.store.read_projection_shared(session).await.unwrap();
    let log = StoreLog(&fx.store);
    let transcript = KernelTranscript(fx.model.clone());
    let cx = RewriteCtx {
        session,
        message,
        model: &fx.model,
        projection: &projection,
        window: Some(ModelWindow {
            max_context: 10_000,
            usage_reporting: false,
            reasoning_replay: hya_proto::context::ReasoningReplayPolicy::None,
            sends_tool_images: false,
        }),
        overhead_tokens: 0,
        pressure: None,
        workdir: &fx.dir,
        config: &serde_json::Value::Null,
        log: &log,
        transcript: &transcript,
        models: None,
        hooks: None,
    };
    let probe = Arc::new(Probe {
        fold,
        seen: Default::default(),
    });
    let extensions: Vec<Arc<dyn Rewriter>> = vec![probe.clone()];
    let changed = kernel::before_round(cx, &extensions).await.unwrap();
    let seen = probe.seen.lock().unwrap().clone();
    (changed, fx.log().await, seen)
}

fn compactions(log: &[Event]) -> Vec<CompactionStrategy> {
    log.iter()
        .filter_map(|event| match event {
            Event::ContextCompacted { strategy, .. } => Some(*strategy),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_extension_that_reduces_enough_spares_the_kernel_rewrite() {
    let (changed, log, seen) = reduce_with("extension-enough", true).await;
    assert!(changed);
    assert_eq!(seen.len(), 1);
    let pressure = seen[0].expect("a window gives a pressure");
    assert_eq!(pressure.threshold, 8_000);
    assert!(pressure.over(), "{pressure:?}");
    assert_eq!(compactions(&log), [CompactionStrategy::Native]);
}

#[tokio::test]
async fn the_kernel_rewrites_after_an_extension_that_did_not_reduce_enough() {
    let (changed, log, seen) = reduce_with("extension-short", false).await;
    assert!(changed);
    assert!(seen[0].is_some_and(Pressure::over));
    assert_eq!(compactions(&log), [CompactionStrategy::Rewrite]);
    // The extension's change lands first, then the kernel's rewrite.
    let probe_marker = log
        .iter()
        .position(|event| matches!(event, Event::TextDelta { delta, .. } if delta == "a note from the probe"))
        .unwrap();
    let rewrite = log
        .iter()
        .position(|event| matches!(event, Event::ContextCompacted { .. }))
        .unwrap();
    assert!(probe_marker < rewrite);
}

#[tokio::test]
async fn a6_harness_rewrite_and_restart() {
    let fx = fixture(
        "a6",
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::Reasoning("look before acting".to_string()),
                FakeStep::ToolCall {
                    name: "bash".to_string(),
                    input: json!({"command": "echo hi"}),
                },
                FakeStep::Usage(TokenUsage {
                    input: 900,
                    output: 20,
                    ..TokenUsage::default()
                }),
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            text("done"),
            text("checked"),
        ])
        .with_max_context(1_000),
    )
    .await;
    let finish = fx.turn("go", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    let envelopes = fx.store.replay(fx.session).await.unwrap();
    let log: Vec<_> = envelopes.iter().map(|e| e.event.clone()).collect();
    assert_paired(&log);

    // The rewrite lands before the next round starts.
    let compacted = log
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::ContextCompacted {
                    strategy: CompactionStrategy::Rewrite,
                    ..
                }
            )
        })
        .expect("harness rewrite");
    let next_step = log
        .iter()
        .position(|event| matches!(event, Event::StepStarted { step: 1, .. }))
        .expect("second round");
    assert!(compacted < next_step);
    let Event::MessageStarted {
        message: marker, ..
    } = &log[compacted - 5]
    else {
        panic!("marker message precedes the compaction record");
    };
    assert!(matches!(
        &log[compacted],
        Event::ContextCompacted { message, .. } if message == marker
    ));

    // Reasoning leaves only with its whole step.
    let reasoning = log
        .iter()
        .find_map(|event| match event {
            Event::ReasoningStart { part, .. } => Some(*part),
            _ => None,
        })
        .unwrap();
    let call_part = log
        .iter()
        .find_map(|event| match event {
            Event::ToolCallRequested { part, .. } => Some(*part),
            _ => None,
        })
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 3);
    let sent = part_ids(&requests[1]);
    assert_eq!(
        sent.contains(&reasoning),
        sent.contains(&call_part),
        "reasoning and its step leave together"
    );
    if sent.contains(&reasoning) {
        assert!(assistant_parts(&requests[1]).any(|part| matches!(
            part,
            Part::Reasoning { text, .. } if text == "look before acting"
        )));
    }

    // Each recorded cut reproduces the request, including the round after the rewrite.
    let mut attempts = 0;
    for (index, envelope) in envelopes.iter().enumerate() {
        let Event::StepStarted { view, .. } = &envelope.event else {
            continue;
        };
        let recorded = view.as_ref().expect("step has a view");
        assert_eq!(recorded.cut, envelopes[index - 1].seq);
        let end = envelopes.partition_point(|entry| entry.seq <= recorded.cut);
        let projection = Projection::from_events(&envelopes[..end]);
        let view = View {
            projection: &projection,
        };
        let rebuilt = kernel::request::request(
            &view,
            &fx.system,
            fx.tools.iter().map(|tool| tool.schema()).collect(),
            &fx.model,
            &AgentName::new(kernel::AGENT),
            &AttachmentData::new(),
            None,
        );
        assert_eq!(
            kernel::request::request_sha256(&rebuilt).unwrap(),
            recorded.request_sha256
        );
        assert_eq!(format!("{rebuilt:?}"), format!("{:?}", requests[attempts]));
        attempts += 1;
    }
    assert_eq!(attempts, requests.len());
}

#[tokio::test]
async fn rewrite_carries_the_latest_todo_result_verbatim() {
    let fx = fixture(
        "rewrite-todos",
        FakeProvider::scripted_turns(vec![
            call(
                "todo",
                json!({"ops": [
                    {"op": "add", "content": "inspect the inputs"},
                    {"op": "add", "content": "check all outputs"},
                    {"op": "status", "id": "2", "status": "in_progress"}
                ]}),
            ),
            call("rewrite", json!({"summary": "continue checking"})),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    fx.turn("check the results", &CancellationToken::new())
        .await
        .unwrap();

    let log = fx.log().await;
    assert_paired(&log);
    let list =
        "Current todos:\n- #1 [pending] inspect the inputs\n- #2 [in_progress] check all outputs";
    assert!(log.windows(2).any(|pair| matches!(
        pair,
        [Event::ToolResult { output, .. }, Event::TodosUpdated { todos, .. }]
            if output == list && todos.len() == 2
    )));
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(!assistant_parts(&requests[2]).any(|part| matches!(
        part, Part::Tool { name, .. } if name.as_str() == "todo"
    )));
    assert!(requests[2].messages.iter().any(|message| matches!(
        message,
        Message::System { content, .. }
            if content.starts_with(COMPACT_CONTEXT_MARKER) && content.ends_with(list)
    )));
}

#[tokio::test]
async fn invalid_todo_batch_is_rolled_back_without_recording_an_update() {
    let fx = fixture(
        "invalid-todos",
        FakeProvider::scripted_turns(vec![
            call(
                "todo",
                json!({"ops": [{"op": "add", "content": "keep me"}]}),
            ),
            call(
                "todo",
                json!({"ops": [
                    {"op": "edit", "id": "1", "content": "undo this"},
                    {"op": "add", "content": "undo this too"},
                    {"op": "remove", "id": "99"}
                ]}),
            ),
            call("todo", json!({"ops": []})),
            text("done"),
        ]),
    )
    .await;
    fx.turn("edit the list", &CancellationToken::new())
        .await
        .unwrap();

    let log = fx.log().await;
    assert_paired(&log);
    assert!(
        matches!(tool_errors(&log).as_slice(), [error] if error.contains("unknown todo id `99`"))
    );
    let updates = log
        .iter()
        .filter(|event| matches!(event, Event::TodosUpdated { .. }));
    assert_eq!(updates.count(), 1);
    let results: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult { output, .. } => output.as_str(),
            _ => None,
        })
        .collect();
    assert_eq!(results, ["Current todos:\n- #1 [pending] keep me"; 2]);
}

#[tokio::test]
async fn todo_ids_stay_allocated_after_their_rows_are_removed() {
    let fx = fixture(
        "removed-todos",
        FakeProvider::scripted_turns(vec![
            call(
                "todo",
                json!({"ops": [
                    {"op": "add", "content": "temporary"},
                    {"op": "remove", "id": "1"}
                ]}),
            ),
            call("todo", json!({"ops": [{"op": "add", "content": "keep"}]})),
            text("done"),
        ]),
    )
    .await;
    fx.turn("replace the list", &CancellationToken::new())
        .await
        .unwrap();
    let log = fx.log().await;
    let projection = fx.store.read_projection(fx.session).await.unwrap();
    let todos = projection.session.todos.as_ref().unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].id, "2");
    assert_eq!(todos[0].content, "keep");
    assert_eq!(
        log.iter()
            .filter(|event| matches!(event, Event::TodosUpdated { .. }))
            .count(),
        1
    );
}

fn init_git(dir: &Path) {
    let output = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test]
async fn edit_rejects_identical_text_without_writing() {
    let fx = fixture(
        "edit-identical",
        FakeProvider::scripted_turns(vec![
            call(
                "edit",
                json!({"path": "a.txt", "old": "hello", "new": "hello"}),
            ),
            text("done"),
        ]),
    )
    .await;
    let path = fx.dir.join("a.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let file = std::fs::File::open(&path).unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000)),
    )
    .unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();

    fx.turn("replace hello with hello", &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        tool_errors(&fx.log().await),
        ["input: `old` and `new` are identical. The file was not changed"]
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}

#[tokio::test]
async fn file_mutations_record_prior_content_and_stamp_after_the_tool() {
    let fx = fixture(
        "file-mutations",
        FakeProvider::scripted_turns(vec![
            call("write", json!({"path": "a.txt", "content": "hello\n"})),
            call(
                "edit",
                json!({"path": "a.txt", "old": "hello", "new": "bye"}),
            ),
            call("bash", json!({"command": "printf 'shell\n' >> a.txt"})),
            // Even a write of identical bytes can change the mtime.
            call("write", json!({"path": "a.txt", "content": "bye\nshell\n"})),
            text("done"),
        ]),
    )
    .await;
    init_git(&fx.dir);
    fx.turn("make the edits", &CancellationToken::new())
        .await
        .unwrap();

    let log = fx.log().await;
    let mut changes = Vec::new();
    let mut sizes = Vec::new();
    for (index, event) in log.iter().enumerate() {
        match event {
            Event::FilesChanged {
                call: Some(call),
                files,
                ..
            } => {
                assert!(
                    matches!(&log[index - 1], Event::ToolResult { call: result, .. } if result == call)
                );
                let [file] = files.as_slice() else {
                    panic!("expected one changed file: {files:?}")
                };
                assert_eq!(Path::new(&file.path), fx.dir.join("a.txt"));
                changes.push(file);
            }
            Event::FilesObserved {
                external, files, ..
            } => {
                assert!(
                    !external,
                    "a tool's changes were reported as external: {files:?}"
                );
                let [file] = files.as_slice() else {
                    panic!("expected one stamp: {files:?}")
                };
                assert_eq!(Path::new(&file.path), fx.dir.join("a.txt"));
                assert!(file.mtime_ns.is_some());
                sizes.push(file.size.unwrap());
            }
            _ => {}
        }
    }
    assert_eq!(sizes, [6, 4, 10, 10]);
    assert_eq!(changes.len(), 3);
    assert_eq!(changes[0].before, FileState::Absent);
    for (file, bytes) in [(changes[1], b"hello\n".as_slice()), (changes[2], b"bye\n")] {
        let FileState::Stored { hash, .. } = &file.before else {
            panic!("missing prior content: {file:?}")
        };
        assert_eq!(
            fx.store
                .file_blob(fx.session, hash)
                .await
                .unwrap()
                .as_deref(),
            Some(bytes)
        );
    }
    assert!(changes[2].observed);
}

#[tokio::test]
async fn outside_change_to_read_file_is_logged_before_the_next_request() {
    let fx = fixture(
        "file-between-rounds",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "a.txt"})),
            call("read", json!({"path": "a.txt"})),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    let a = fx.dir.join("a.txt");
    std::fs::write(&a, "old").unwrap();
    let publish = |envelope: Envelope| {
        if matches!(envelope.event, Event::ToolResult { output, .. } if output["output"] == "old") {
            std::fs::write(&a, "outside update").unwrap();
            std::fs::write(fx.dir.join("b.txt"), "unseen").unwrap();
        }
    };
    fx.turn_with_publish("read twice", &CancellationToken::new(), &publish)
        .await
        .unwrap();

    let log = fx.store.replay(fx.session).await.unwrap();
    let external: Vec<_> = log
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::FilesObserved {
                external: true,
                files,
                ..
            } => Some((envelope.seq, files)),
            _ => None,
        })
        .collect();
    #[cfg(target_os = "linux")]
    let external = {
        let [(baseline_seq, baseline), change] = external.as_slice() else {
            panic!("expected the initial W and one incremental event: {external:?}")
        };
        assert!(baseline_seq < &change.0);
        assert_eq!(baseline.len(), 1);
        assert_eq!(baseline[0].size, Some(3));
        vec![*change]
    };
    let [(seq, files)] = external.as_slice() else {
        panic!("expected one external event: {external:?}")
    };
    #[cfg(target_os = "linux")]
    {
        assert_eq!(files.len(), 2);
        assert_eq!(Path::new(&files[1].path), fx.dir.join("b.txt"));
        assert_eq!(files[1].size, Some(6));
    }
    #[cfg(not(target_os = "linux"))]
    assert_eq!(files.len(), 1);
    let file = &files[0];
    assert_eq!(Path::new(&file.path), a);
    assert_eq!(file.size, Some(14));
    let next = log.iter().find(|e| e.seq.0 == seq.0 + 1).unwrap();
    assert!(
        matches!(&next.event, Event::StepStarted { step: 1, view: Some(view), .. } if view.cut == *seq)
    );
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        !assistant_parts(&requests[1]).any(|part| matches!(part, Part::Tool {
        state: ToolPartState::Completed { output, .. }, ..
    } if output["output"] == "outside update"))
    );
    assert!(
        assistant_parts(&requests[2]).any(|part| matches!(part, Part::Tool {
        state: ToolPartState::Completed { output, .. }, ..
    } if output["output"] == "outside update"))
    );
}

#[tokio::test]
async fn file_stamps_survive_reopening_the_database() {
    let dir = std::env::temp_dir().join(format!("hya-kernel-file-restart-{}", SessionId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("hya.db");
    let db = db.to_str().unwrap();
    let a = dir.join("a.txt");
    std::fs::write(&a, "old").unwrap();
    let mut fx = fixture_with_store(
        dir.clone(),
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "a.txt"})),
            text("done"),
            text("back"),
        ]),
        SessionStore::connect(db).await.unwrap(),
    )
    .await;
    fx.turn("read before restart", &CancellationToken::new())
        .await
        .unwrap();
    let session = fx.session;
    // Close all connections and discard the store's in-process projection.
    fx.watches = Default::default();
    drop(fx.store);
    fx.store = SessionStore::connect(db).await.unwrap();
    std::fs::remove_file(&a).unwrap();

    fx.turn("resume after restart", &CancellationToken::new())
        .await
        .unwrap();
    let log = fx.log().await;
    let observed: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::FilesObserved {
                external: true,
                files,
                ..
            } => Some(files.as_slice()),
            _ => None,
        })
        .collect();
    #[cfg(target_os = "linux")]
    let observed = {
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].len(), 1, "session database files entered W");
        assert_eq!(observed[0][0].size, Some(3));
        &observed[1..]
    };
    #[cfg(not(target_os = "linux"))]
    let observed = observed.as_slice();
    let [[file]] = observed else {
        panic!("expected one deletion: {observed:?}")
    };
    assert_eq!(Path::new(&file.path), a);
    assert_eq!(file.mtime_ns, None);
    assert_eq!(file.size, None);
    assert_eq!(
        serde_json::to_value(file).unwrap(),
        json!({"path": a, "mtime_ns": null, "size": null})
    );
    assert_eq!(
        fx.store
            .read_projection_shared(session)
            .await
            .unwrap()
            .session
            .files[&file.path],
        *file
    );
}

struct ShellWithConcurrentEdit(Arc<dyn Tool>);

#[async_trait::async_trait]
impl Tool for ShellWithConcurrentEdit {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        self.0.schema()
    }

    async fn execute(
        &self,
        ctx: &ToolCtx,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        if input["command"] == "sleep 0.05" {
            // Poll the real Bash tool first so the shell is running when the
            // other task edits the workspace.
            let (outcome, ()) = tokio::join!(biased;
                self.0.execute(ctx, input),
                async {
                    tokio::fs::write(ctx.workdir.join("a.txt"), "outside").await.unwrap();
                    tokio::fs::write(ctx.workdir.join("b.txt"), "unseen").await.unwrap();
                },
            );
            outcome
        } else {
            self.0.execute(ctx, input).await
        }
    }
}

#[tokio::test]
async fn shell_edits_outside_git_are_the_commands_own_observation() {
    let fx = fixture(
        "file-shell-no-git",
        FakeProvider::scripted_turns(vec![
            call("write", json!({"path": "a.txt", "content": "hello\n"})),
            call("bash", json!({"command": "printf 'shell\\n' >> a.txt"})),
            call("read", json!({"path": "a.txt"})),
            text("done"),
        ]),
    )
    .await;
    fx.turn("write, append, and read", &CancellationToken::new())
        .await
        .unwrap();

    let a = fx.dir.join("a.txt");
    let log = fx.log().await;
    assert!(
        !log.iter()
            .any(|event| matches!(event, Event::FilesObserved { external: true, .. })),
        "{log:?}"
    );
    let own: Vec<&Path> = log
        .iter()
        .filter_map(|event| match event {
            Event::FilesObserved {
                external: false,
                files,
                ..
            } => Some(files.iter().map(|file| Path::new(&file.path))),
            _ => None,
        })
        .flatten()
        .collect();
    // write, bash (no Git: stat of the known file), read.
    assert_eq!(own, [a.as_path(), a.as_path(), a.as_path()]);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "hello\nshell\n");
}

#[tokio::test]
async fn outside_edits_during_a_shell_call_are_recorded_as_its_changes() {
    let mut fx = fixture(
        "file-sleep",
        FakeProvider::scripted_turns(vec![
            call("write", json!({"path": "a.txt", "content": "hello\n"})),
            call("bash", json!({"command": "printf 'shell\n' >> a.txt"})),
            call("bash", json!({"command": "sleep 0.05"})),
            call("read", json!({"path": "a.txt"})),
            text("done"),
        ]),
    )
    .await;
    init_git(&fx.dir);
    let bash = fx
        .tools
        .iter_mut()
        .find(|tool| tool.name() == "bash")
        .unwrap();
    *bash = Arc::new(ShellWithConcurrentEdit(Arc::clone(bash)));
    fx.turn("write, append, wait, and read", &CancellationToken::new())
        .await
        .unwrap();

    let a = fx.dir.join("a.txt");
    let b = fx.dir.join("b.txt");
    let log = fx.log().await;
    let changed: Vec<Vec<&Path>> = log
        .iter()
        .filter_map(|event| match event {
            Event::FilesChanged { files, .. } => {
                Some(files.iter().map(|file| Path::new(&file.path)).collect())
            }
            _ => None,
        })
        .collect();
    // Git measures the shell's combined delta, so the outside edits made
    // during the wait are recorded as that command's changes.
    assert_eq!(
        changed,
        [
            vec![a.as_path()],
            vec![a.as_path()],
            vec![a.as_path(), b.as_path()]
        ]
    );
    assert!(
        !log.iter()
            .any(|event| matches!(event, Event::FilesObserved { external: true, .. })),
        "{log:?}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn independent_workspace_views_reconcile_moves_same_stamp_and_a_cold_restart() {
    use std::fs::{self, FileTimes};
    use std::os::unix::fs::symlink;

    let mut a = fixture(
        "notify-readers",
        FakeProvider::scripted_turns(vec![text("ok"); 4]),
    )
    .await;
    let mut b = fixture_with_store(
        a.dir.clone(),
        FakeProvider::scripted_turns(vec![text("ok"); 3]),
        a.store.clone(),
    )
    .await;
    b.watches = a.watches.clone();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("private"), "outside").unwrap();
    symlink(outside.path(), a.dir.join("link")).unwrap();
    let old = a.dir.join("before/f");
    let new = a.dir.join("after/f");
    fs::create_dir(old.parent().unwrap()).unwrap();
    fs::write(&old, "one").unwrap();
    let cancel = CancellationToken::new();
    a.turn("start", &cancel).await.unwrap();
    b.turn("start", &cancel).await.unwrap();
    fs::rename(old.parent().unwrap(), new.parent().unwrap()).unwrap();
    a.turn("see move", &cancel).await.unwrap();
    // Rename away and back before the lagging reader queries. The inode is
    // unchanged, but inotify detached its original directory descriptor.
    fs::rename(new.parent().unwrap(), outside.path().join("away")).unwrap();
    fs::rename(outside.path().join("away"), new.parent().unwrap()).unwrap();
    b.turn("catch up", &cancel).await.unwrap();
    let modified = fs::metadata(&new).unwrap().modified().unwrap();
    fs::write(&new, "two").unwrap();
    fs::File::open(&new)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    a.turn("see equal stamp", &cancel).await.unwrap();
    b.turn("watch still attached", &cancel).await.unwrap();

    let observations = |log: Vec<Event>| {
        log.into_iter()
            .filter_map(|e| match e {
                Event::FilesObserved {
                    external: true,
                    files,
                    ..
                } => Some(files),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let first = observations(a.log().await);
    let second = observations(b.log().await);
    assert_eq!(first.len(), 3);
    assert_eq!(second.len(), 3);
    assert_eq!(second[2], first[2]);
    assert_eq!(first[0].len(), 1);
    assert_eq!(first[1].len(), 2);
    assert_eq!(first[1], second[1]);
    assert!(
        first[1]
            .iter()
            .any(|s| s.path == old.to_str().unwrap() && s.size.is_none())
    );
    assert_eq!(first[2].len(), 1);
    assert_eq!(
        first[2][0],
        *first[1]
            .iter()
            .find(|s| s.path == new.to_str().unwrap())
            .unwrap()
    );

    a.watches = Default::default();
    fs::remove_file(&new).unwrap();
    fs::write(a.dir.join("offline"), "new").unwrap();
    a.turn("cold resume", &cancel).await.unwrap();
    let recovered = observations(a.log().await);
    assert_eq!(recovered[3].len(), 2);
    assert!(
        recovered[3]
            .iter()
            .any(|s| s.path == new.to_str().unwrap() && s.size.is_none())
    );
    assert!(
        recovered[3]
            .iter()
            .any(|s| s.path.ends_with("/offline") && s.size == Some(3))
    );
}

/// A route over a fake that claims `model` only (every model when `None`),
/// replays reasoning by `policy`, and records `provider_data` on each
/// reasoning part the fake streams.
struct Route {
    fake: Arc<FakeProvider>,
    model: Option<ModelRef>,
    policy: ReasoningReplayPolicy,
    provider_data: Option<serde_json::Value>,
}

#[async_trait::async_trait]
impl Provider for Route {
    fn id(&self) -> &str {
        self.fake.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        if self.model.as_ref().is_some_and(|claimed| claimed != model) {
            return None;
        }
        self.fake.capabilities(model)
    }

    fn reasoning_replay_policy(&self) -> ReasoningReplayPolicy {
        self.policy
    }

    fn sends_tool_images(&self, caps: &Capabilities) -> bool {
        self.fake.sends_tool_images(caps)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        use futures::StreamExt as _;
        let data = self.provider_data.clone();
        let stream = self.fake.stream(req, session, message).await?;
        Ok(Box::pin(stream.map(move |event| {
            event.map(|event| match event {
                Event::ReasoningEnd {
                    session,
                    message,
                    part,
                    provider_data: None,
                } => Event::ReasoningEnd {
                    session,
                    message,
                    part,
                    provider_data: data.clone(),
                },
                event => event,
            })
        })))
    }
}

/// A summary-only Responses reasoning item: replayed by hya, but not
/// carried by an endpoint that needs its encrypted content.
fn summary_item() -> serde_json::Value {
    json!({
        "id": "rsn-1",
        "type": "reasoning",
        "summary": [{"type": "summary_text", "text": "step thought"}],
    })
}

/// A fixture whose only route replays reasoning by `policy` and records
/// `provider_data` on reasoning parts.
async fn route_fixture(
    name: &str,
    provider: FakeProvider,
    policy: ReasoningReplayPolicy,
    provider_data: Option<serde_json::Value>,
) -> Fixture {
    let mut fx = fixture(name, provider).await;
    fx.router = ProviderRouter::new().with(Arc::new(Route {
        fake: fx.provider.clone(),
        model: None,
        policy,
        provider_data,
    }));
    fx
}

/// A fixture on a route that replays each reasoning part as a provider item.
async fn replaying_fixture(name: &str, provider: FakeProvider) -> Fixture {
    route_fixture(
        name,
        provider,
        ReasoningReplayPolicy::ProviderData,
        Some(summary_item()),
    )
    .await
}

/// A round that thinks, makes `calls`, and reports `usage`.
fn thought(calls: Vec<FakeStep>, usage: TokenUsage) -> Vec<FakeStep> {
    let mut steps = vec![FakeStep::Reasoning("thinking".into())];
    steps.extend(calls);
    steps.push(FakeStep::Usage(TokenUsage {
        output: usage.reasoning + 20,
        ..usage
    }));
    steps.push(FakeStep::Finish(FinishReason::ToolCalls));
    steps
}

fn usage(input: u64, reasoning: u64) -> TokenUsage {
    TokenUsage {
        input,
        reasoning,
        ..TokenUsage::default()
    }
}

/// A `todo` round with `input` prompt tokens and `reasoning` thinking tokens.
fn todo_round(input: u64, reasoning: u64) -> Vec<FakeStep> {
    thought(call("todo", json!({"ops": []})), usage(input, reasoning))
}

const REASONING_NOTICE: &str = "[hya] Reasoning replay notice:";

/// Indexes of the requests that carry the dropped-reasoning notice.
fn reasoning_notice_requests(fx: &Fixture) -> Vec<usize> {
    let carries = |message: &Message| match message {
        Message::User { parts, .. } => parts.iter().any(
            |part| matches!(part, Part::Text { text, .. } if text.starts_with(REASONING_NOTICE)),
        ),
        Message::System { content, .. } => content.starts_with(REASONING_NOTICE),
        Message::Assistant { .. } => false,
    };
    (0..)
        .zip(fx.provider.requests())
        .filter(|(_, request)| request.messages.iter().any(carries))
        .map(|(index, _)| index)
        .collect()
}

/// Step 1's prompt grew by less than half of step 0's replayed reasoning: the
/// notice ends step 2's request only, and a later drop in the same turn sends
/// no second one.
#[tokio::test]
async fn dropped_reasoning_sends_one_notice_per_turn() {
    let fx = replaying_fixture(
        "reasoning-dropped",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            todo_round(1300, 2000),
            todo_round(1400, 2000),
            // Dropped again, compared with step 3.
            todo_round(1500, 2000),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    assert_eq!(
        fx.turn("work it out", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );

    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 7);
    assert!(matches!(
        requests[2].messages.last(),
        Some(Message::User { parts, .. }) if matches!(
            parts.as_slice(),
            [Part::Text { text, .. }] if text.starts_with(REASONING_NOTICE)
        )
    ));
    assert_eq!(reasoning_notice_requests(&fx), vec![2]);
}

/// Rounds the check cannot compare, whose reasoning hya did not replay, or
/// whose prompt kept the reasoning, never get the notice.
#[tokio::test]
async fn reasoning_drop_check_skips_uncomparable_rounds() {
    use ReasoningReplayPolicy::{None as NoReplay, ProviderData};
    let unknown = thought(
        call("todo", json!({"ops": []})),
        TokenUsage {
            reasoning_unknown: true,
            ..usage(1000, 2000)
        },
    );
    let replayed = Some(summary_item());
    for (case, policy, provider_data, first, second) in [
        (
            "kept",
            ProviderData,
            replayed.clone(),
            todo_round(1000, 2000),
            todo_round(3200, 2000),
        ),
        (
            "no-replay",
            NoReplay,
            replayed.clone(),
            todo_round(1000, 2000),
            todo_round(1200, 2000),
        ),
        // The route replays provider items, but the reasoning had none.
        (
            "not-replayed",
            ProviderData,
            None,
            todo_round(1000, 2000),
            todo_round(1200, 2000),
        ),
        (
            "unknown",
            ProviderData,
            replayed.clone(),
            unknown,
            todo_round(1200, 2000),
        ),
        (
            "below-threshold",
            ProviderData,
            replayed.clone(),
            todo_round(1000, 500),
            todo_round(1200, 500),
        ),
    ] {
        let fx = route_fixture(
            &format!("reasoning-skip-{case}"),
            FakeProvider::scripted_turns(vec![first, second, text("done"), text("checked")]),
            policy,
            provider_data,
        )
        .await;
        fx.turn("work it out", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(fx.provider.requests().len(), 4, "{case}");
        assert!(reasoning_notice_requests(&fx).is_empty(), "{case}");
    }
}

/// A request that attaches a tool-result image, or the one after it that
/// replaces the image with a note, is not compared across.
#[tokio::test]
async fn reasoning_drop_check_skips_tool_image_rounds() {
    let read = || call("read", json!({"path": "board.data"}));
    for (case, script) in [
        // Step 1 attaches the image: compared with step 0, which had none.
        (
            "current",
            vec![
                thought(read(), usage(1000, 2000)),
                todo_round(1200, 2000),
                text("done"),
                text("checked"),
            ],
        ),
        // Step 1 attaches the image; step 2 sends the note in its place.
        (
            "previous",
            vec![
                read(),
                todo_round(1000, 2000),
                todo_round(1200, 2000),
                text("done"),
                text("checked"),
            ],
        ),
    ] {
        let steps = script.len();
        let fx = replaying_fixture(
            &format!("reasoning-image-{case}"),
            FakeProvider::scripted_turns(script).with_tool_images(),
        )
        .await;
        write_board(&fx.dir);
        fx.turn("look at the board", &CancellationToken::new())
            .await
            .unwrap();
        let requests = fx.provider.requests();
        assert_eq!(requests.len(), steps, "{case}");
        assert!(attaches_board(&requests[1]), "{case}");
        assert!(!attaches_board(&requests[2]), "{case}");
        assert!(reasoning_notice_requests(&fx).is_empty(), "{case}");
    }
}

const BOARD_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=";

fn write_board(dir: &Path) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    std::fs::write(dir.join("board.data"), STANDARD.decode(BOARD_PNG).unwrap()).unwrap();
}

fn attaches_board(request: &CompletionRequest) -> bool {
    AnthropicMessagesProtocol
        .encode_for_model(request, None, true)
        .unwrap()
        .to_string()
        .contains(BOARD_PNG)
}

/// A rewrite between two rounds starts the next request from a new marker,
/// so its smaller prompt is not compared.
#[tokio::test]
async fn reasoning_drop_check_skips_a_rewrite() {
    let fx = replaying_fixture(
        "reasoning-rewrite",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 0),
            thought(
                call("rewrite", json!({"summary": "state so far"})),
                usage(1000, 2000),
            ),
            todo_round(1200, 2000),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    fx.turn("work it out", &CancellationToken::new())
        .await
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 5);
    assert!(matches!(
        requests[2].messages.first(),
        Some(Message::System { content, .. }) if content.starts_with(COMPACT_CONTEXT_MARKER)
    ));
    assert!(reasoning_notice_requests(&fx).is_empty());
}

/// The goal checkpoint ends step 1's request only: neither step 1 (which
/// carries it) nor step 2 (which drops it) is compared with its predecessor.
#[tokio::test]
async fn reasoning_drop_check_skips_transient_notice_rounds() {
    // Step 0's prompt plus output reaches 60% of the window before step 1,
    // below the 80% that would rewrite.
    let fx = replaying_fixture(
        "reasoning-notice",
        FakeProvider::scripted_turns(vec![
            todo_round(10_000, 2000),
            todo_round(10_200, 2000),
            todo_round(10_300, 2000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    fx.turn("work it out", &CancellationToken::new())
        .await
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 5);
    let checkpoint = |message: Option<&Message>| {
        matches!(message, Some(Message::System { content, .. })
            if content.starts_with("[hya] Goal checkpoint:"))
    };
    assert!(checkpoint(requests[1].messages.last()));
    assert!(!requests[2].messages.iter().any(|m| checkpoint(Some(m))));
    assert!(reasoning_notice_requests(&fx).is_empty());
}

/// A rewrite before the request that would carry the notice removes it
/// unsent; the next drop in the turn then sends it.
#[tokio::test]
async fn reasoning_notice_removed_unsent_is_sent_on_the_next_drop() {
    // Step 1's prompt plus output passes 80% of the window: the kernel
    // rewrites before step 2.
    let fx = replaying_fixture(
        "reasoning-notice-rewritten",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 2000),
            todo_round(1200, 20_000),
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    fx.turn("work it out", &CancellationToken::new())
        .await
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 6);
    assert!(matches!(
        requests[2].messages.first(),
        Some(Message::System { content, .. }) if content.starts_with(COMPACT_CONTEXT_MARKER)
    ));
    let logged = fx
        .log()
        .await
        .iter()
        .filter(|event| matches!(event, Event::TextDelta { delta, .. } if delta.starts_with(REASONING_NOTICE)))
        .count();
    assert_eq!(logged, 2, "one notice after step 1, one after step 3");
    assert_eq!(reasoning_notice_requests(&fx), vec![4]);
}

/// A partless round after the notice gets the empty-turn notice, and the
/// next request carries that instead of the reasoning notice again.
#[tokio::test]
async fn empty_turn_replaces_an_unanswered_reasoning_notice() {
    let fx = replaying_fixture(
        "reasoning-notice-empty-turn",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            vec![FakeStep::Finish(FinishReason::Stop)],
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    fx.turn("work it out", &CancellationToken::new())
        .await
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 5);
    assert!(matches!(
        requests[3].messages.last(),
        Some(Message::User { parts, .. }) if matches!(
            parts.as_slice(),
            [Part::Text { text, .. }] if text.starts_with("[hya] Your last turn ended with thinking only")
        )
    ));
    assert_eq!(reasoning_notice_requests(&fx), vec![2]);
}

/// Every round is served by `fallback` instead of the requested model, as a
/// full-mode model fallback does; tools run as in bare mode. With `live`,
/// rounds open on that router, which the scope's router predates.
struct Fallback<'a> {
    scope: &'a kernel::Scope<'a>,
    fx: &'a Fixture,
    fallback: ModelRef,
    ctx: ToolCtx,
    live: Option<ProviderRouter>,
}

#[async_trait::async_trait]
impl kernel::ToolGate for Fallback<'_> {
    async fn call(&mut self, mut call: kernel::ToolCall) -> Result<(), CoreError> {
        let tool = self
            .fx
            .tools
            .iter()
            .find(|tool| tool.name() == call.name)
            .unwrap();
        let output = tool
            .execute(&self.ctx, std::mem::take(&mut call.input))
            .await
            .unwrap();
        let (session, message) = (self.scope.session, self.scope.message);
        self.scope
            .emit
            .emit(
                session,
                vec![Event::ToolResult {
                    session,
                    message,
                    part: call.part,
                    call: call.call,
                    output,
                    time_ms: 0,
                }],
            )
            .await
    }

    async fn cancelled(&mut self, _calls: Vec<kernel::ToolCall>) -> Result<(), CoreError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl kernel::Harness for Fallback<'_> {
    async fn before_round(&mut self, _projection: &Projection) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn request(&mut self, view: &View<'_>) -> Result<CompletionRequest, CoreError> {
        Ok(kernel::request::request(
            view,
            &self.fx.system,
            self.fx.tools.iter().map(|tool| tool.schema()).collect(),
            &self.fx.model,
            &AgentName::new(kernel::AGENT),
            &AttachmentData::new(),
            None,
        ))
    }

    async fn open(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<Option<(EventStream, ModelRef)>, CoreError> {
        let mut request = request.clone();
        request.model = self.fallback.clone();
        let stream = self
            .live
            .as_ref()
            .unwrap_or(self.scope.provider)
            .stream(request, self.scope.session, self.scope.message)
            .await?;
        Ok(Some((stream, self.fallback.clone())))
    }

    async fn failed(&mut self, _failure: &kernel::Failure) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn collected(&mut self, _round: &kernel::Round) -> Result<(), CoreError> {
        Ok(())
    }

    async fn before_end(&mut self) -> Result<bool, CoreError> {
        Ok(false)
    }

    fn done(&self) -> bool {
        false
    }

    fn served_route(&self, model: &ModelRef) -> Option<(ReasoningReplayPolicy, bool)> {
        let live = self.live.as_ref()?;
        Some((
            live.reasoning_replay_policy(model),
            live.sends_tool_images(model),
        ))
    }
}

/// The served fallback's route, not the requested model's, decides whether
/// reasoning was replayed and images attached.
#[tokio::test]
async fn reasoning_drop_check_uses_the_serving_route() {
    let fallback = ModelRef::new("fake/fallback");
    for (case, script, notice) in [
        // Only the fallback replays reasoning: its drop is reported.
        (
            "replay",
            vec![todo_round(1000, 2000), todo_round(1200, 2000), text("done")],
            vec![2],
        ),
        // The fallback route exists only on the live router the harness
        // reports, not on the scope's.
        (
            "live",
            vec![todo_round(1000, 2000), todo_round(1200, 2000), text("done")],
            vec![2],
        ),
        // Only the fallback attaches images: step 1 carries one.
        (
            "image",
            vec![
                thought(
                    call("read", json!({"path": "board.data"})),
                    usage(1000, 2000),
                ),
                todo_round(1200, 2000),
                text("done"),
            ],
            vec![],
        ),
    ] {
        let mut fx = fixture(
            &format!("reasoning-fallback-{case}"),
            FakeProvider::scripted_turns(script).with_tool_images(),
        )
        .await;
        let requested = Arc::new(Route {
            fake: Arc::new(FakeProvider::scripted_turns(Vec::new())),
            model: Some(fx.model.clone()),
            policy: ReasoningReplayPolicy::None,
            provider_data: Some(summary_item()),
        });
        let served = ProviderRouter::new()
            .with(requested.clone())
            .with(Arc::new(Route {
                fake: fx.provider.clone(),
                model: Some(fallback.clone()),
                policy: ReasoningReplayPolicy::ProviderData,
                provider_data: Some(summary_item()),
            }));
        let live = if case == "live" {
            fx.router = ProviderRouter::new().with(requested);
            Some(served)
        } else {
            fx.router = served;
            None
        };
        write_board(&fx.dir);
        let message = fx.begin("work it out").await;
        let emit = StoreEmit {
            store: &fx.store,
            publish: &|_| {},
        };
        let scope = kernel::Scope {
            watches: &fx.watches,
            store: &fx.store,
            emit: &emit,
            complete: None,
            provider: &fx.router,
            session: fx.session,
            message,
        };
        let cancel = CancellationToken::new();
        let mut harness = Fallback {
            scope: &scope,
            fx: &fx,
            fallback: fallback.clone(),
            ctx: tool_ctx(&fx.dir, &cancel),
            live,
        };
        kernel::drive(&scope, &mut harness, &cancel).await.unwrap();
        let requests = fx.provider.requests();
        assert_eq!(requests.len(), 3, "{case}");
        assert!(requests.iter().all(|request| request.model == fallback));
        if case == "image" {
            assert!(attaches_board(&requests[1]));
        }
        assert_eq!(reasoning_notice_requests(&fx), notice, "{case}");
    }
}

/// A prompt that only begins like the notice does not count as its
/// delivery: the kernel's own notice is still sent after a drop.
#[tokio::test]
async fn reasoning_notice_is_sent_after_a_prompt_quoting_it() {
    let prompt = format!("{REASONING_NOTICE} pasted from an old log");
    // Step 0's request ends with the prompt, which reads as a transient
    // notice, so the drop is measured from step 1 to step 2.
    let fx = replaying_fixture(
        "reasoning-notice-quoted",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 0),
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    fx.turn(&prompt, &CancellationToken::new()).await.unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 5);
    assert!(matches!(
        requests[3].messages.last(),
        Some(Message::User { parts, .. }) if matches!(
            parts.as_slice(),
            [Part::Text { text, .. }] if text.starts_with(REASONING_NOTICE) && *text != prompt
        )
    ));
}

/// `GoogleThought` routes replay compact `thoughtSignature` tokens without
/// billing prior `thoughtsTokenCount` into `promptTokenCount`, so stable
/// prompts across tool steps never emit `REASONING_DROPPED_NOTICE`.
#[tokio::test]
async fn google_thought_replay_does_not_trigger_reasoning_dropped_notice() {
    let google_thought = Some(json!({
        "type": hya_proto::context::GOOGLE_THOUGHT_TYPE,
        "thought_signature": "sig_g1"
    }));
    let fx = route_fixture(
        "reasoning-skip-google-thought",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            text("done"),
            text("checked"),
        ]),
        ReasoningReplayPolicy::GoogleThought,
        google_thought,
    )
    .await;
    fx.turn("work it out", &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(fx.provider.requests().len(), 4);
    assert!(reasoning_notice_requests(&fx).is_empty());
}
