//! Isolated process: the lifecycle SQL counter is process-global.
#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_core::{AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine};
use hya_proto::{Event, FinishReason, MessageId, ModelRef, PartId, SessionId, ToolCallId};
use hya_provider::{
    Capabilities, CompletionRequest, DevProvider, EventStream, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::{SessionStore, bench};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio::sync::mpsc;

struct Deltas(mpsc::UnboundedSender<u64>);

#[async_trait]
impl Provider for Deltas {
    fn id(&self) -> &str {
        "hya"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        DevProvider::new().capabilities(model)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let part = PartId::new();
        let reasoning = PartId::new();
        let call = ToolCallId::new();
        let mut events = vec![Event::TextStart {
            session,
            message,
            part,
        }];
        events.extend((0..32).map(|_| Event::TextDelta {
            session,
            message,
            part,
            delta: "x".into(),
        }));
        events.push(Event::TextEnd {
            session,
            message,
            part,
        });
        events.push(Event::ReasoningStart {
            reason: None,
            session,
            message,
            part: reasoning,
        });
        events.extend((0..32).map(|_| Event::ReasoningDelta {
            session,
            message,
            part: reasoning,
            delta: "x".into(),
        }));
        events.push(Event::ReasoningEnd {
            session,
            message,
            part: reasoning,
            provider_data: None,
        });
        events.extend((0..32).map(|_| Event::ToolInputDelta {
            session,
            message,
            part,
            call,
            name: "read".into(),
            delta: "x".into(),
        }));
        let mut events = events.into_iter();
        let counts = self.0.clone();
        let mut before = None;
        Ok(Box::pin(futures::stream::poll_fn(move |_| {
            if let Some(previous) = before.take() {
                counts
                    .send(bench::snapshot().statements - previous)
                    .unwrap();
            }
            let event = events.next();
            if matches!(
                event,
                Some(
                    Event::TextDelta { .. }
                        | Event::ReasoningDelta { .. }
                        | Event::ToolInputDelta { .. }
                )
            ) {
                before = Some(bench::snapshot().statements);
            }
            std::task::Poll::Ready(event.map(Ok))
        })))
    }
}

#[tokio::test]
async fn actor_stream_deltas_execute_no_sql() {
    let dir = support::TestDir::new("stream-claim-sql");
    let path = dir.path().join("sessions.db");
    let store = SessionStore::connect(path.to_str().unwrap()).await.unwrap();
    bench::warm(&store).await.unwrap();
    let (counts, mut measured) = mpsc::unbounded_channel();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(Deltas(counts))));
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: "hya-task".into(),
        model: "hya/offline".into(),
        workdir: "/tmp".into(),
        system_prompt: "test".into(),
        reasoning: None,
    };
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: "hya-main".into(),
            model: agent.model.clone(),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let root_turn = engine.try_begin_turn(root).unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    supervisor
        .register_existing_resident(root, child, "worker-1".into(), agent, Some("stream".into()))
        .await
        .unwrap();
    let mut sql = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..96 {
            sql.push(measured.recv().await.unwrap());
        }
        while engine.turn_active(child) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    engine.begin_drain(hya_proto::FinishCause::Shutdown);
    drop(root_turn);
    supervisor
        .drain(hya_proto::FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert_eq!(
        sql,
        vec![0; 96],
        "SQL statements per text/reasoning/tool-input delta"
    );
    let projection = engine.read_projection(child).await.unwrap();
    assert!(
        projection
            .session
            .messages
            .iter()
            .any(|message| message.finish == Some(FinishReason::Stop))
    );
}
