//! Deadline instructions must name a tool the Workflow actor can call.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use async_trait::async_trait;
use hya_core::{
    AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine, SubagentGovernor,
    SubagentLimits, WorkflowRunContext, WorkflowStatus, run_workflow,
};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct NoticeProvider {
    inner: FakeProvider,
    report_offered: Mutex<BTreeMap<SessionId, bool>>,
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
        self.report_offered.lock().unwrap().insert(
            session,
            req.tools.iter().any(|tool| tool.name.as_str() == "report"),
        );
        self.inner.stream(req, session, message).await
    }
}

#[tokio::test]
async fn workflow_worker_and_verifier_deadline_notices_follow_offered_report_tool() {
    // Short budgets begin in Final; no wall-clock delay or global test race.
    hya_tool::deadline::install(Duration::from_secs(40));
    let mut wrong_notices = Vec::new();
    for (governed, connected, allowed) in [
        (false, true, true),
        (true, true, true),
        (true, false, true),
        (true, true, false),
    ] {
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
        let provider = Arc::new(NoticeProvider {
            inner: FakeProvider::scripted_turns(vec![tool.clone(), answer.clone(), tool, answer]),
            report_offered: Mutex::default(),
        });
        let (permission, _) = PermissionPlane::new(PermissionRules::new(vec![
            Rule::new(Action::Tool, "glob", Mode::Allow),
            Rule::new(Action::Glob, "*", Mode::Allow),
        ]));
        let tools = Arc::new(ToolRegistry::builtins());
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
            Arc::new(hya_core::RuntimeRegistry::new(
                ToolRegistry::builtins(),
                Arc::new(hya_core::AgentCatalog::new(Arc::new(catalog)).unwrap()),
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
        let lifecycle_task = tokio::spawn(hya_core::run_lifecycle_service(
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
            let expected = governed && connected && allowed;
            if notices[0].contains("call `report`") != expected {
                wrong_notices.push(format!("governed={governed}, connected={connected}, allowed={allowed}, child={child}: {}", notices[0]));
            }
        }
        supervisor
            .drain(hya_proto::FinishCause::Shutdown, hya_core::DRAIN_DEADLINE)
            .await;
        lifecycle_stop.cancel();
        lifecycle_task.await.unwrap();
    }
    assert!(
        wrong_notices.is_empty(),
        "unusable report instructions: {wrong_notices:#?}"
    );
}
