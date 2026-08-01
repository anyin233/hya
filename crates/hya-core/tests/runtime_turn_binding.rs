#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, ToolName, ToolPartState, ToolSchema};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError, ToolPermission,
    ToolRegistry,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use support::TestDir;

struct CountingTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name),
            description: format!("{} marker", self.name),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(json!({ "generation": self.name }))
    }
}

struct GatedProvider {
    call: AtomicUsize,
    requests: mpsc::UnboundedSender<(usize, CompletionRequest)>,
    release_first: Arc<Notify>,
}

#[async_trait]
impl Provider for GatedProvider {
    fn id(&self) -> &str {
        "gated"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        let call = self.call.fetch_add(1, Ordering::Relaxed);
        let _ = self.requests.send((call, request));
        let script = if call == 0 {
            self.release_first.notified().await;
            vec![
                FakeStep::ToolCall {
                    name: "generation_n".to_string(),
                    input: json!({}),
                },
                FakeStep::ToolCall {
                    name: "skill".to_string(),
                    input: json!({ "name": "skill_n" }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![FakeStep::Finish(FinishReason::Stop)]
        };
        let events = FakeProvider::materialize(&script, session, message);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

fn schema_names(request: &CompletionRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect()
}

#[tokio::test]
async fn admitted_turn_uses_one_binding_for_prompt_schema_skill_and_dispatch() {
    let workdir = TestDir::new("turn-binding");
    workdir.write_skill("skill_n");
    let old_calls = Arc::new(AtomicUsize::new(0));
    let new_calls = Arc::new(AtomicUsize::new(0));
    let tools = ToolRegistry::builtins();
    tools
        .register_with_permission(
            Arc::new(CountingTool {
                name: "generation_n",
                calls: old_calls.clone(),
            }),
            ToolPermission::ReadOnly,
        )
        .unwrap();

    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let release_first = Arc::new(Notify::new());
    let router = Arc::new(ProviderRouter::new().with(Arc::new(GatedProvider {
        call: AtomicUsize::new(0),
        requests: request_tx,
        release_first: release_first.clone(),
    })));
    let rules = PermissionRules::new(vec![Rule::new(Action::Skill, "*", Mode::Allow)]);
    let (permission, _asks) = PermissionPlane::new(rules);
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::test_runtime(Arc::new(tools)),
        permission,
        EventBus::default(),
    ));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("gated/model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("gated/model"),
        system_prompt: "base prompt".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };
    engine
        .admit_user_prompt(session, "first".to_string())
        .await
        .unwrap();

    let turn_engine = engine.clone();
    let turn_agent = agent.clone();
    let first_turn = tokio::spawn(async move {
        turn_engine
            .run_turn(session, &turn_agent, CancellationToken::new())
            .await
    });
    let (call, first_request) = request_rx.recv().await.expect("first provider request");
    assert_eq!(call, 0);

    workdir.remove_skill("skill_n");
    workdir.write_skill("skill_n_plus_1");
    let published = engine
        .refresh_runtime(|candidate| {
            candidate.remove_tool("generation_n");
            candidate.register_tool_with_permission(
                Arc::new(CountingTool {
                    name: "generation_n_plus_1",
                    calls: new_calls.clone(),
                }),
                ToolPermission::ReadOnly,
            )?;
            candidate.refresh_skills(workdir.path());
            Ok(())
        })
        .unwrap();
    release_first.notify_one();
    assert_eq!(first_turn.await.unwrap().unwrap(), FinishReason::Stop);
    let (call, second_round_request) = request_rx.recv().await.expect("second provider request");
    assert_eq!(call, 1);

    for request in [&first_request, &second_round_request] {
        let schemas = schema_names(request);
        assert!(schemas.contains(&"generation_n".to_string()));
        assert!(!schemas.contains(&"generation_n_plus_1".to_string()));
        let system = request.system.as_deref().unwrap_or_default();
        assert!(system.contains("skill_n"));
        assert!(!system.contains("skill_n_plus_1"));
    }
    assert_eq!(old_calls.load(Ordering::Relaxed), 1);
    assert_eq!(new_calls.load(Ordering::Relaxed), 0);

    let first_projection = engine.read_projection(session).await.unwrap();
    assert!(first_projection.session.messages.iter().any(|message| {
        message.parts.iter().any(|part| {
            matches!(
                part,
                hya_proto::PartProjection::Tool {
                    name,
                    state: ToolPartState::Completed { output, .. },
                    ..
                } if name.as_str() == "skill"
                    && output["title"].as_str() == Some("Loaded skill: skill_n")
            )
        })
    }));
    let first_bindings = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(generation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(first_bindings.len(), 1);
    assert!(first_bindings[0].get() < published.get());

    engine
        .admit_user_prompt(session, "second".to_string())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let (call, next_turn_request) = request_rx.recv().await.expect("next turn provider request");
    assert_eq!(call, 2);
    let schemas = schema_names(&next_turn_request);
    assert!(!schemas.contains(&"generation_n".to_string()));
    assert!(schemas.contains(&"generation_n_plus_1".to_string()));
    let system = next_turn_request.system.as_deref().unwrap_or_default();
    assert!(!system.contains("skill_n\n"));
    assert!(system.contains("skill_n_plus_1"));

    let bindings = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(generation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(bindings, vec![first_bindings[0], published]);
}
