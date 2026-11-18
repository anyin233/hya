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
    AgentName, CompactionStrategy, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId,
    Projection, Role, SessionId, SessionKind, TokenUsage, ToolPartState,
};
use hya_provider::{
    COMPACT_CONTEXT_MARKER, CompletionRequest, FakeProvider, FakeStep, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct Fixture {
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
        let publish = |_| {};
        let emit = StoreEmit {
            store: &self.store,
            publish: &publish,
        };
        let ctx = tool_ctx(&self.dir, cancel);
        let finish = kernel::run(kernel::Run {
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
        })
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
        todo: hya_tool::TodoPlane::default(),
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
