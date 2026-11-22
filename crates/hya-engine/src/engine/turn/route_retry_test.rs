use super::*;
use crate::workflow::{
    WorkflowModelRoute, WorkflowModelRouteCandidate, WorkflowRouteRecorder, WorkflowTurnRouteSpec,
};
use crate::{CreateSession, EventBus};
use hya_proto::{MemberId, WorkflowMemberRole, WorkflowRouteFailureClass, WorkflowRunId};
use hya_provider::{
    Capabilities, FakeProvider, FakeStep, Provider, ProviderRouter, ReasoningEffort,
};
use std::sync::Mutex;

#[derive(Default)]
struct DisconnectingProvider(Mutex<Vec<ModelRef>>);

#[async_trait::async_trait]
impl Provider for DisconnectingProvider {
    fn id(&self) -> &str {
        "routed"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            ..Default::default()
        })
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let mut calls = self.0.lock().unwrap_or_else(|e| e.into_inner());
        calls.push(req.model.clone());
        if calls.len() == 1 {
            return Err(ProviderError::Transport("offline".into()));
        }
        let text = |value: &str| {
            FakeProvider::materialize(&[FakeStep::Text(value.into())], session, message)
                .into_iter()
                .map(Ok)
        };
        if req.model.as_str() == "a" {
            return Ok(Box::pin(futures::stream::iter(text("recovered primary"))));
        }
        if calls.len() == 2 {
            let partial =
                text("partial").chain([Err(ProviderError::StreamInterrupted("EOF".into()))]);
            return Ok(Box::pin(futures::stream::iter(partial)));
        }
        Ok(Box::pin(futures::stream::iter([Err(
            ProviderError::Decode("invalid frame".into()),
        )])))
    }
}

#[tokio::test]
async fn interrupted_workflow_retries_only_its_serving_candidate_and_finalizes_once()
-> Result<(), Box<dyn std::error::Error>> {
    let provider = Arc::new(DisconnectingProvider::default());
    let engine = SessionEngine::new(
        hya_store::SessionStore::connect_memory().await?,
        Arc::new(ProviderRouter::new().with(provider.clone())),
        crate::test_support::runtime(hya_tool::ToolRegistry::builtins()),
        PermissionPlane::new(hya_tool::PermissionRules::default()).0,
        EventBus::default(),
    );
    let agent = AgentSpec {
        name: "hya-main".into(),
        model: "a".into(),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: ".".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await?;
    engine.admit_user_prompt(session, "go".into()).await?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let route = WorkflowTurnRoute::new(WorkflowTurnRouteSpec {
        route: WorkflowModelRoute {
            candidates: ["a", "b"]
                .map(|name| WorkflowModelRouteCandidate {
                    model: name.into(),
                    reasoning: ReasoningEffort::Medium,
                })
                .into(),
            selected_index: 0,
        },
        session,
        run: Some(WorkflowRunId::new()),
        stage: "work".into(),
        member: MemberId::new(),
        role: WorkflowMemberRole::Worker,
        iteration: 0,
        recorder: Some(WorkflowRouteRecorder::new(tx)),
    });
    let turn = engine.run_turn_with_external_dirs_and_claim(
        session,
        &agent,
        TurnActivation::Root,
        TurnRequestContext::new(CancellationToken::new(), &[], None, None, Some(route)),
    );
    let record = async {
        let mut outcomes = Vec::new();
        while let Some(record) = rx.recv().await {
            outcomes.push(record.event);
            assert!(record.ack.send(Ok(())).is_ok());
        }
        outcomes
    };
    let (result, outcomes) = tokio::join!(turn, record);
    assert!(
        matches!(result, Err(CoreError::Provider(ref error)) if matches!(error.as_ref(), ProviderError::Decode(_)))
    );
    assert_eq!(
        &*provider.0.lock().unwrap_or_else(|e| e.into_inner()),
        &[ModelRef::new("a"), ModelRef::new("b"), ModelRef::new("b")]
    );
    assert!(
        matches!(&outcomes[..], [Event::WorkflowStageRouteOutcome { candidate_index: 1, step: 0, model, failure_class: WorkflowRouteFailureClass::Decode, .. }] if model.as_str() == "b")
    );
    Ok(())
}
