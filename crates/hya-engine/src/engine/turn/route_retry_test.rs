use super::*;
use crate::{CreateSession, EventBus};
use hya_provider::{
    Capabilities, FakeProvider, FakeStep, Provider, ProviderRouter, ReasoningEffort,
};
use hya_runtime_abi::{TurnRouteFailureClass, TurnRouteOutcome};
use std::sync::Mutex;

#[derive(Default)]
struct DisconnectingProvider {
    calls: Mutex<Vec<ModelRef>>,
    cancel_on_interruption: Option<CancellationToken>,
    cancel_on_reopen: Option<CancellationToken>,
}

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
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
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
            let partial = FakeProvider::materialize(
                &[FakeStep::Reasoning("thinking".into())],
                session,
                message,
            )
            .into_iter()
            .map(Ok)
            .chain([Err(ProviderError::StreamInterrupted("EOF".into()))]);
            // collect drops the interrupted stream immediately before persisting
            // StepFinished. Cancelling on drop places the signal before failed().
            let guard = self
                .cancel_on_interruption
                .clone()
                .map(CancellationToken::drop_guard);
            return Ok(Box::pin(futures::StreamExt::map(
                futures::stream::iter(partial),
                move |item| {
                    let _keep_until_stream_drops = &guard;
                    item
                },
            )));
        }
        if let Some(cancel) = &self.cancel_on_reopen {
            cancel.cancel();
            return Err(ProviderError::Transport("pinned reopen failed".into()));
        }
        Ok(Box::pin(futures::stream::iter([Err(
            ProviderError::Decode("invalid frame".into()),
        )])))
    }
}

#[tokio::test]
async fn interrupted_workflow_retries_only_its_serving_candidate_and_finalizes_once()
-> Result<(), Box<dyn std::error::Error>> {
    check_interrupted_route(false, false).await
}

#[tokio::test]
async fn cancelled_interrupted_step_finalizes_once_before_backoff()
-> Result<(), Box<dyn std::error::Error>> {
    check_interrupted_route(true, false).await
}

#[tokio::test]
async fn cancellation_at_pinned_reopen_failure_records_cancelled()
-> Result<(), Box<dyn std::error::Error>> {
    check_interrupted_route(false, true).await
}

async fn check_interrupted_route(
    cancel_on_interruption: bool,
    cancel_on_reopen: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cancel = CancellationToken::new();
    let provider = Arc::new(DisconnectingProvider {
        cancel_on_interruption: cancel_on_interruption.then(|| cancel.clone()),
        cancel_on_reopen: cancel_on_reopen.then(|| cancel.clone()),
        ..Default::default()
    });
    let engine = SessionEngine::new(
        hya_store::SessionStore::connect_memory().await?,
        Arc::new(ProviderRouter::new().with(provider.clone())),
        crate::test_support::runtime(hya_tool_loader::builtins()),
        PermissionPlane::new(hya_tool::PermissionRules::default()).0,
        EventBus::default(),
    )
    .with_round_retry_waits(&[std::time::Duration::ZERO; 8]);
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
    let route = ExplicitTurnRoute::new(ExplicitModelRoute {
        candidates: ["a", "b"]
            .map(|name| ExplicitModelRouteCandidate {
                model: name.into(),
                reasoning: ReasoningEffort::Medium,
            })
            .into(),
        selected_index: 0,
    });
    let result = engine
        .run_turn_with_external_dirs_and_claim(
            session,
            &agent,
            TurnActivation::Root,
            TurnRequestContext::new(cancel, &[], None, None, Some(route.clone())),
        )
        .await;
    let outcomes = route.outcomes();
    if cancel_on_interruption || cancel_on_reopen {
        if cancel_on_reopen {
            assert!(matches!(result, Ok(FinishReason::Cancelled)), "{result:?}");
        } else {
            assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
        }
        assert!(
            engine
                .store()
                .replay(session)
                .await?
                .iter()
                .any(|env| matches!(
                    env.event,
                    Event::StepFinished {
                        finish: FinishReason::Error,
                        ..
                    }
                ))
        );
        assert_eq!(
            provider
                .calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            if cancel_on_reopen { 3 } else { 2 }
        );
        assert!(matches!(
            &outcomes[..],
            [TurnRouteOutcome {
                failure_class: TurnRouteFailureClass::Cancelled,
                ..
            }]
        ));
        return Ok(());
    }
    assert!(
        matches!(result, Err(CoreError::Provider(ref error)) if matches!(error.as_ref(), ProviderError::Decode(_)))
    );
    assert_eq!(
        &*provider.calls.lock().unwrap_or_else(|e| e.into_inner()),
        &[ModelRef::new("a"), ModelRef::new("b"), ModelRef::new("b")]
    );
    assert!(
        matches!(&outcomes[..], [TurnRouteOutcome { candidate_index: 1, step: 0, model, failure_class: TurnRouteFailureClass::Decode, .. }] if model.as_str() == "b")
    );
    Ok(())
}
