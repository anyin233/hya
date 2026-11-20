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
    ProviderRouter, ReasoningEffort,
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

    async fn turn_with_publish(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
        publish: &(dyn Fn(Envelope) + Send + Sync),
    ) -> Result<FinishReason, CoreError> {
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
        FakeProvider::scripted_turns(vec![call("bash", json!({"command": "true"})), text("done")])
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

/// One stream call of [`Flaky`]: `None` plays the fake script, `Some` streams
/// those steps without their last event, then fails with that error.
type Attempt = Option<(Vec<FakeStep>, ProviderError)>;

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
        let Some((partial, error)) = self.failures.lock().unwrap().pop_front().flatten() else {
            return self.fake.stream(req, session, message).await;
        };
        let mut events = FakeProvider::materialize(&partial, session, message);
        events.pop();
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
        ],
        vec![
            None,
            Some((Vec::new(), server_error("upstream unreachable"))),
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
async fn a_stream_interruption_before_any_part_is_retried() {
    let (fx, flaky) = flaky_fixture(
        "retry-interrupted-stream",
        vec![text("done")],
        vec![
            Some((
                Vec::new(),
                ProviderError::StreamInterrupted("stalled stream: no SSE frame within 300s".into()),
            )),
            Some((Vec::new(), server_error("upstream unreachable"))),
        ],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5, 10]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_stream_interruption_after_a_part_is_not_retried() {
    let (fx, flaky) = flaky_fixture(
        "interrupted-stream-after-reasoning",
        vec![text("unreached")],
        vec![Some((
            vec![FakeStep::Reasoning("half a thought".into())],
            ProviderError::StreamInterrupted("stalled stream: no SSE frame within 300s".into()),
        ))],
    )
    .await;
    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("the failed attempt persisted reasoning");
    assert!(matches!(
        error,
        CoreError::Provider(error) if matches!(*error, ProviderError::StreamInterrupted(_))
    ));
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    assert!(fx.log().await.iter().any(|event| matches!(
        event,
        Event::ReasoningReplace { text, .. } if text == "half a thought"
    )));
}

#[tokio::test]
async fn non_transient_round_failure_ends_the_run() {
    let (fx, flaky) = flaky_fixture(
        "retry-invalid",
        vec![text("unreached")],
        vec![Some((
            Vec::new(),
            ProviderError::HttpStatus {
                status: 400,
                message: "in-stream error (invalid_request_error): bad".into(),
                retry_after: None,
            },
        ))],
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
        .map(|attempt| Some((Vec::new(), server_error(&format!("attempt {attempt}")))))
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
        .map(|attempt| Some((Vec::new(), server_error(&format!("attempt {attempt}")))))
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
async fn a_transient_failure_after_reasoning_is_not_retried() {
    let (fx, flaky) = flaky_fixture(
        "retry-after-reasoning",
        vec![text("unreached")],
        vec![Some((
            vec![FakeStep::Reasoning("half a thought".into())],
            server_error("upstream unreachable"),
        ))],
    )
    .await;
    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("the failed attempt persisted reasoning");
    assert_eq!(
        error.to_string(),
        "http status 500: in-stream error (server_error): upstream unreachable"
    );
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    assert!(fx.log().await.iter().any(|event| matches!(
        event,
        Event::ReasoningReplace { text, .. } if text == "half a thought"
    )));
}

#[tokio::test]
async fn a_completed_round_resets_the_retry_count() {
    let failures = (1..=18)
        .map(|attempt| {
            (attempt != 9).then(|| (Vec::new(), server_error(&format!("attempt {attempt}"))))
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
        vec![Some((Vec::new(), server_error("upstream unreachable")))],
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
                "Image file (image/png, 80 bytes).\\n\\n[The image of this result \
                 (image/png) is not attached to this request"
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
