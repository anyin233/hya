//! Deadline instructions must name a tool the Workflow actor can call, also
//! in the notice after a reasoning-only round was cut at a deadline stage.
#![allow(clippy::unwrap_used, clippy::expect_used)]

// Own binary: `hya_tool::deadline::install` sets a process-wide deadline.
#[path = "it/support/mod.rs"]
mod support;

use async_trait::async_trait;
use futures::StreamExt;
use hya_engine::{
    AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine, SubagentGovernor,
    SubagentLimits, WorkflowRunContext, WorkflowStatus, run_workflow,
};
use hya_proto::context::REASONING_DEADLINE_NOTICE_PREFIX;
use hya_proto::{
    AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct NoticeProvider {
    inner: FakeProvider,
    /// Each session's first round only reasons, without end.
    think_first: bool,
    report_offered: Mutex<BTreeMap<SessionId, bool>>,
    /// The last user text of each request, by session.
    last_user: Mutex<Vec<(SessionId, String)>>,
}

#[async_trait]
impl Provider for NoticeProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let first = self
            .report_offered
            .lock()
            .unwrap()
            .insert(
                session,
                req.tools.iter().any(|tool| tool.name.as_str() == "report"),
            )
            .is_none();
        let last_user = req.messages.iter().rev().find_map(|message| match message {
            Message::User { parts, .. } => Some(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        });
        self.last_user
            .lock()
            .unwrap()
            .push((session, last_user.unwrap_or_default()));
        if self.think_first && first {
            let part = PartId::new();
            let events = [
                Event::ReasoningStart {
                    session,
                    message,
                    part,
                    reason: None,
                },
                Event::ReasoningDelta {
                    session,
                    message,
                    part,
                    delta: "weighing the options".into(),
                },
            ];
            return Ok(futures::stream::iter(events.map(Ok::<_, ProviderError>))
                .chain(futures::stream::pending())
                .boxed());
        }
        self.inner.stream(req, session, message).await
    }
}

#[tokio::test]
async fn workflow_worker_and_verifier_deadline_notices_follow_offered_report_tool() {
    // A 30 s budget begins in Over (announced as Final) with no time left
    // past its 30 s grace, so `reasoning_grace()` is zero: a cut needs no
    // wall-clock wait, and there is no global test race.
    hya_tool::deadline::install(Duration::from_secs(30));
    let mut wrong_notices = Vec::new();
    for (think_first, governed, connected, allowed) in [false, true].into_iter().flat_map(|think| {
        [
            (think, false, true, true),
            (think, true, true, true),
            (think, true, false, true),
            (think, true, true, false),
        ]
    }) {
        let tool = vec![
            FakeStep::ToolCall {
                name: "glob".into(),
                input: json!({"pattern":"*"}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ];
        let answer = vec![
            FakeStep::Text(r#"{"met":true,"reason":"done"}"#.into()),
            FakeStep::Finish(FinishReason::Stop),
        ];
        // Thinking first, the cut notice replaces the tool result's notice.
        let scripts = if think_first {
            vec![answer.clone(), answer]
        } else {
            vec![tool.clone(), answer.clone(), tool, answer]
        };
        let provider = Arc::new(NoticeProvider {
            inner: FakeProvider::scripted_turns(scripts),
            think_first,
            report_offered: Mutex::default(),
            last_user: Mutex::default(),
        });
        let (permission, _) = PermissionPlane::new(PermissionRules::new(vec![
            Rule::new(Action::Tool, "glob", Mode::Allow),
            Rule::new(Action::Glob, "*", Mode::Allow),
        ]));
        let tools = Arc::new(hya_tool_loader::builtins());
        let runtime = if allowed {
            support::test_runtime(tools)
        } else {
            let source = hya_bundle::BundleSource::new(
                "denied-report",
                vec![hya_bundle::SourceFile::new(
                    "bundle.yaml",
                    "kind: Bundle\nidentity: { id: acme/denied-report, version: 1.0.0, publisher: acme }\nchannels:\n  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [send, steer, follow_up, resident_mail], scope: vertical, retention: team_session }\n",
                )],
            );
            let prepared = hya_bundle::prepare_package(source).unwrap();
            let catalog = hya_bundle::BundleCatalog::from_prepared(prepared.bundles()).unwrap();
            Arc::new(hya_engine::RuntimeRegistry::new(
                hya_tool_loader::builtins(),
                Arc::new(hya_engine::AgentCatalog::new(Arc::new(catalog)).unwrap()),
            ))
        };
        let (lifecycle, lifecycle_rx) = hya_tool::LifecyclePlane::new();
        let engine = SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(provider.clone())),
            runtime,
            permission,
            EventBus::default(),
        );
        let engine = if connected {
            engine.with_lifecycle(lifecycle)
        } else {
            engine
        };
        let engine = Arc::new(if governed {
            engine.with_governor(SubagentGovernor::new(SubagentLimits::default()))
        } else {
            engine
        });
        let dir = support::TestDir::new("workflow-deadline");
        let base = AgentSpec {
            name: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            system_prompt: String::new(),
            workdir: dir.path().into(),
            reasoning: None,
        };
        let root = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: dir.path().display().to_string(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        let supervisor = ResidentSupervisor::start(engine.clone());
        let lifecycle_stop = CancellationToken::new();
        let lifecycle_task = tokio::spawn(hya_engine::run_lifecycle_service(
            engine.clone(),
            supervisor.clone(),
            lifecycle_rx,
            lifecycle_stop.clone(),
        ));
        let workflow = hya_workflow::compile(hya_workflow::WorkflowSource::new("deadline.hya.md", "---\nkind: Workflow\nname: deadline\ndescription: Offered report capability\nnodes:\n  loop:\n    agent: hya-task\n    directive: Work.\n    mode: loop\n    verify:\n      agent: hya-reviewer\n      until: Finished.\n      max_iterations: 2\n---\nflowchart TD\n  loop\n")).unwrap();
        let result = run_workflow(
            engine.clone(),
            root,
            &workflow,
            WorkflowRunContext {
                binding: engine.bind_runtime(dir.path()).unwrap(),
                caller: "hya-main".into(),
                base_agent: base,
                inputs: BTreeMap::new(),
                resident_supervisor: Some(supervisor.clone()),
                routing: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.status, WorkflowStatus::Completed);
        let members = engine.read_projection(root).await.unwrap().session.members;
        assert_eq!(members.len(), 2, "worker and verifier must both execute");
        for member in members {
            let child = member.child.unwrap();
            assert_eq!(provider.report_offered.lock().unwrap()[&child], governed);
            let events = engine.replay(child).await.unwrap();
            let notices = events
                .clone()
                .into_iter()
                .filter_map(|e| match e.event {
                    Event::ToolResult { output, .. }
                        if output.to_string().contains("HEADLESS RUN") =>
                    {
                        Some(output.to_string())
                    }
                    Event::TextDelta { delta, .. }
                        if delta.starts_with(REASONING_DEADLINE_NOTICE_PREFIX) =>
                    {
                        Some(delta)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                notices.len(),
                1,
                "child {child}: {:?}",
                events
                    .iter()
                    .filter(|e| matches!(
                        e.event,
                        Event::ToolResult { .. } | Event::ToolError { .. }
                    ))
                    .collect::<Vec<_>>()
            );
            if think_first {
                assert!(notices[0].starts_with(REASONING_DEADLINE_NOTICE_PREFIX));
                // The round after the cut sends the notice as its last user message.
                let sent: Vec<_> = provider
                    .last_user
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(session, _)| *session == child)
                    .map(|(_, text)| text.clone())
                    .collect();
                assert_eq!(sent.len(), 2, "child {child}: {sent:?}");
                assert_eq!(sent[1], notices[0]);
            }
            let expected = governed && connected && allowed;
            if notices[0].contains("call `report`") != expected {
                wrong_notices.push(format!("think_first={think_first}, governed={governed}, connected={connected}, allowed={allowed}, child={child}: {}", notices[0]));
            }
        }
        supervisor
            .drain(hya_proto::FinishCause::Shutdown, hya_engine::DRAIN_DEADLINE)
            .await;
        lifecycle_stop.cancel();
        lifecycle_task.await.unwrap();
    }
    assert!(
        wrong_notices.is_empty(),
        "unusable report instructions: {wrong_notices:#?}"
    );
}
