//! Kernel invariants (ADR-0033, plan §5-A): pairing, failures and
//! cancellation as observations, and the rewrite primitive.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hya_core::CoreError;
use hya_core::kernel::{self, StoreEmit, View, request::AttachmentData};
use hya_proto::{
    AgentName, CompactionStrategy, Envelope, Event, FileState, FinishReason, Message, MessageId,
    ModelRef, Part, PartId, Projection, Role, SessionId, SessionKind, TokenUsage, ToolPartState,
};
use hya_provider::{
    AnthropicMessagesProtocol, COMPACT_CONTEXT_MARKER, CompletionRequest, FakeProvider, FakeStep,
    Protocol, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[test]
fn bare_deadline_notices() {
    // Install the process-wide clock in a child so no other kernel test sees it.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "bare_deadline_notices_child", "--nocapture"])
        .env("HYA_BARE_DEADLINE_TEST", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn bare_deadline_notices_child() {
    if std::env::var_os("HYA_BARE_DEADLINE_TEST").is_none() {
        return;
    }
    let fx = fixture(
        "deadline-notice",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            call("todo", json!({"ops": []})),
            vec![FakeStep::Text("done".into())],
        ]),
    )
    .await;
    // A 40-second budget is already in Final, without sleeping.
    hya_tool::deadline::install(Duration::from_secs(40));
    assert_eq!(
        fx.turn("write the deliverable", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 3);
    let Some(Message::User { parts, .. }) = requests[0].messages.last() else {
        panic!("expected the deadline notice at the end of the first request");
    };
    let [Part::Text { text, .. }] = parts.as_slice() else {
        panic!("expected a single deadline notice");
    };
    let left = text
        .strip_prefix("HEADLESS RUN — FINAL ")
        .and_then(|rest| rest.split_once("s: "))
        .and_then(|(seconds, _)| seconds.parse::<u64>().ok())
        .expect("Final notice with seconds left");
    assert!((30..40).contains(&left), "{text}");
    assert!(text.contains("of 40s used. Write any missing deliverable"));
    assert!(
        requests
            .iter()
            .all(|req| req.system.as_deref() == Some(fx.system.as_str()))
    );
    for req in &requests[1..] {
        assert!(!format!("{:?}", req.messages).contains("HEADLESS RUN — FINAL"));
    }
    let log = fx.log().await;
    let logged: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::TextDelta { delta, .. } if delta.starts_with("HEADLESS RUN — FINAL ") => {
                Some(delta)
            }
            _ => None,
        })
        .collect();
    assert_eq!(logged, vec![text]);
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
            json!({"exit_code": 0, "output": "hello"})
        ]
    );
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
    assert_eq!(requests.len(), 4);
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

#[tokio::test]
async fn read_image_reaches_the_model_from_live_and_replayed_history() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let png = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=";
    let fx = fixture(
        "read-image",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "board.data"})),
            call("rewrite", json!({"summary": "saw the board"})),
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
    let wire = AnthropicMessagesProtocol
        .encode_for_model(&requests[1], None, true)
        .unwrap();
    let result = &wire["messages"][2]["content"][0];
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

    let envelopes = fx.store.replay(fx.session).await.unwrap();
    let next_round = envelopes
        .iter()
        .position(|entry| matches!(entry.event, Event::StepStarted { step: 1, .. }))
        .unwrap();
    let projection = Projection::from_events(&envelopes[..next_round]);
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
        wire
    );

    assert!(
        matches!(assistant_parts(&requests[2]).collect::<Vec<_>>().as_slice(),
            [Part::Tool { name, state: ToolPartState::Completed { .. }, .. }] if name.as_str() == "rewrite"
        )
    );
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
    assert_eq!(requests.len(), 5);
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
        Event::ToolResult { output, .. } if output["output"] == "one\n"
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
    assert_eq!(asked, [0, 0, 1, 0]);
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
    assert_eq!(asked, [0, 0, 1, 0, 0]);
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
    assert_eq!(requests.len(), 2);
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
    assert_eq!(requests.len(), 3);
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
        if matches!(envelope.event, Event::ToolResult { output, .. } if output["content"] == "old")
        {
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
    assert_eq!(requests.len(), 3);
    assert!(
        !assistant_parts(&requests[1]).any(|part| matches!(part, Part::Tool {
        state: ToolPartState::Completed { output, .. }, ..
    } if output["content"] == "outside update"))
    );
    assert!(
        assistant_parts(&requests[2]).any(|part| matches!(part, Part::Tool {
        state: ToolPartState::Completed { output, .. }, ..
    } if output["content"] == "outside update"))
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
