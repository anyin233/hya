//! Regression coverage for engine-level provider round retries.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream;
use hya_core::{AgentSpec, CoreError, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, PartId, SessionId, ToolCallId, ToolName,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
enum Item {
    Reasoning,
    Text(&'static str),
    Tool,
    Error,
}
struct ScriptedProvider {
    scripts: Mutex<VecDeque<Vec<Item>>>,
    opens: Arc<Mutex<usize>>,
}
impl ScriptedProvider {
    fn new(scripts: Vec<Vec<Item>>) -> (Self, Arc<Mutex<usize>>) {
        let opens = Arc::new(Mutex::new(0));
        (
            Self {
                scripts: Mutex::new(VecDeque::from(scripts)),
                opens: opens.clone(),
            },
            opens,
        )
    }
}
#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted-retry"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }
    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        *self.opens.lock().unwrap() += 1;
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        let mut out = Vec::new();
        for item in script {
            match item {
                Item::Reasoning => out.extend(
                    FakeProvider::materialize(
                        &[FakeStep::Reasoning("thinking".into())],
                        session,
                        message,
                    )
                    .into_iter()
                    .map(Ok),
                ),
                Item::Text(text) => out.extend(
                    FakeProvider::materialize(&[FakeStep::Text(text.into())], session, message)
                        .into_iter()
                        .map(Ok),
                ),
                Item::Tool => out.push(Ok(Event::ToolCallRequested {
                    session,
                    message,
                    part: PartId::new(),
                    call: ToolCallId::new(),
                    name: ToolName::new("bash"),
                    input: json!({}),
                })),
                Item::Error => out.push(Err(ProviderError::Decode(
                    "error decoding response body".into(),
                ))),
            }
        }
        if !out
            .iter()
            .any(|e| matches!(e, Ok(Event::MessageFinished { .. })))
            && !out.iter().any(std::result::Result::is_err)
        {
            out.extend(
                FakeProvider::materialize(
                    &[FakeStep::Finish(FinishReason::Stop)],
                    session,
                    message,
                )
                .into_iter()
                .map(Ok),
            );
        }
        Ok(Box::pin(stream::iter(out)))
    }
}

async fn fixture(
    scripts: Vec<Vec<Item>>,
) -> (Arc<SessionEngine>, AgentSpec, SessionId, Arc<Mutex<usize>>) {
    let (provider, opens) = ScriptedProvider::new(scripts);
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        PermissionPlane::new(PermissionRules::default()).0,
        EventBus::default(),
    ));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "go".into())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".into(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent, session, opens)
}

#[tokio::test]
async fn retries_reasoning_only_failure_and_commits_final_text() {
    let (engine, agent, session, opens) = fixture(vec![
        vec![Item::Reasoning, Item::Error],
        vec![Item::Text("final")],
    ])
    .await;
    let result = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
    assert_eq!(result.unwrap(), FinishReason::Stop);
    assert_eq!(*opens.lock().unwrap(), 2);
    let events = engine.store().replay(session).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e.event,
                Event::MessageFinished {
                    role: hya_proto::Role::Assistant,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e.event,
                Event::StepFinished {
                    finish: FinishReason::Error,
                    ..
                }
            ))
            .count(),
        1
    );
    // Each attempt owns a distinct step: started 0 (failed), 1 (succeeded),
    // and every StepFinished closes a step that was started.
    let steps: Vec<(&str, u32, Option<FinishReason>)> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::StepStarted { step, .. } => Some(("start", *step, None)),
            Event::StepFinished { step, finish, .. } => Some(("finish", *step, Some(*finish))),
            _ => None,
        })
        .collect();
    assert_eq!(
        steps,
        vec![
            ("start", 0, None),
            ("finish", 0, Some(FinishReason::Error)),
            ("start", 1, None),
            ("finish", 1, Some(FinishReason::Stop)),
        ]
    );
    assert!(
        format!(
            "{:?}",
            engine.store().read_projection(session).await.unwrap()
        )
        .contains("final")
    );
}

#[tokio::test]
async fn does_not_retry_after_text() {
    let (engine, agent, session, opens) =
        fixture(vec![vec![Item::Text("partial"), Item::Error]]).await;
    assert!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(*opens.lock().unwrap(), 1);
}

#[tokio::test]
async fn three_reasoning_failures_are_terminal_error_once() {
    let (engine, agent, session, opens) = fixture(vec![
        vec![Item::Reasoning, Item::Error],
        vec![Item::Reasoning, Item::Error],
        vec![Item::Reasoning, Item::Error],
    ])
    .await;
    assert!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(*opens.lock().unwrap(), 3);
    let events = engine.store().replay(session).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e.event,
                Event::MessageFinished {
                    role: hya_proto::Role::Assistant,
                    finish: FinishReason::Error,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_during_backoff_is_prompt() {
    let (engine, agent, session, opens) = fixture(vec![
        vec![Item::Reasoning, Item::Error],
        vec![Item::Reasoning, Item::Error],
    ])
    .await;
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let task = {
        let e = engine.clone();
        let c = cancel.clone();
        tokio::spawn(async move { e.run_turn(session, &agent, c).await })
    };
    while *opens.lock().unwrap() == 0 {
        tokio::task::yield_now().await;
    }
    cancel.cancel();
    assert!(matches!(
        task.await.unwrap(),
        Ok(FinishReason::Cancelled) | Err(CoreError::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn does_not_retry_after_tool_call() {
    let (engine, agent, session, opens) = fixture(vec![vec![Item::Tool, Item::Error]]).await;
    assert!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(*opens.lock().unwrap(), 1);
}
