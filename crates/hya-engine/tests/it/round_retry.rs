//! Regression coverage for engine-level provider round retries.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream;
use hya_engine::kernel::{self, View, request::AttachmentData};
use hya_engine::{AgentSpec, CoreError, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Projection, SessionId, ToolCallId,
    ToolName,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
enum Item {
    Reasoning,
    EmptyReasoning,
    BilledReasoning,
    Text(&'static str),
    Tool,
    Error,
    Interrupted,
    Fatal,
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
                Item::EmptyReasoning => out.push(Ok(Event::ReasoningStart {
                    session,
                    message,
                    part: PartId::new(),
                    reason: None,
                })),
                Item::BilledReasoning => out.extend(
                    FakeProvider::materialize(
                        &[
                            FakeStep::Usage(hya_proto::TokenUsage {
                                output: 2,
                                reasoning: 2,
                                ..Default::default()
                            }),
                            FakeStep::Finish(FinishReason::Stop),
                        ],
                        session,
                        message,
                    )
                    .into_iter()
                    .map(Ok),
                ),
                Item::Reasoning => out.extend(
                    FakeProvider::materialize(
                        &[FakeStep::Reasoning("thinking".into())],
                        session,
                        message,
                    )
                    .into_iter()
                    .map(|mut event| {
                        if let Event::ReasoningEnd { provider_data, .. } = &mut event {
                            *provider_data = Some(json!({"id": "prior-step"}));
                        }
                        Ok(event)
                    }),
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
                    provider_data: None,
                })),
                Item::Interrupted => out.push(Err(ProviderError::StreamInterrupted(
                    "connection closed".into(),
                ))),
                Item::Fatal => out.push(Err(ProviderError::Decode("invalid schema".into()))),
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
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
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
    let views: Vec<_> = events
        .iter()
        .filter_map(|entry| match &entry.event {
            Event::StepStarted { view, .. } => Some(view.as_ref().expect("attempt has a view")),
            _ => None,
        })
        .collect();
    assert_eq!(views.len(), 2);
    assert_eq!(views[0], views[1]);
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
async fn full_mode_request_replays_to_recorded_view() {
    let provider = Arc::new(FakeProvider::scripted_turns(vec![vec![FakeStep::Text(
        "done".into(),
    )]]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(provider.clone())),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        PermissionPlane::new(PermissionRules::default()).0,
        EventBus::default(),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "full replay system prompt".into(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.display().to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "go".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let log = engine.store().replay(session).await.unwrap();
    let (index, recorded) = log
        .iter()
        .enumerate()
        .find_map(|(index, entry)| match &entry.event {
            Event::StepStarted { view, .. } => {
                Some((index, view.as_ref().expect("step has a view")))
            }
            _ => None,
        })
        .expect("one provider step");
    assert!(recorded.cut < log[index].seq);
    let end = log.partition_point(|entry| entry.seq <= recorded.cut);
    let projection = Projection::from_events(&log[..end]);
    let view = View {
        projection: &projection,
    };
    assert_eq!(view.cut(), recorded.cut);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    // The full harness's resolved prompt and schemas are inputs to replay.
    let rebuilt = kernel::request::request(
        &view,
        sent.system.as_deref().expect("full-mode system prompt"),
        sent.tools.clone(),
        &agent.model,
        &agent.name,
        &AttachmentData::new(),
        None,
    );
    assert_eq!(
        kernel::request::request_sha256(&rebuilt).unwrap(),
        recorded.request_sha256
    );
    assert_eq!(
        kernel::request::request_sha256(sent).unwrap(),
        recorded.request_sha256
    );
}

#[tokio::test]
async fn interrupted_partial_text_is_deleted_before_retry() -> Result<(), CoreError> {
    let (engine, agent, session, opens) = fixture(vec![
        vec![Item::Text("partial"), Item::Interrupted],
        vec![Item::Text("final answer")],
    ])
    .await;
    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await?,
        FinishReason::Stop
    );
    assert_eq!(*opens.lock().unwrap_or_else(|e| e.into_inner()), 2);
    let events = engine.store().replay(session).await?;
    let deleted = events
        .iter()
        .position(|e| matches!(e.event, Event::PartDeleted { .. }));
    let retry = events
        .iter()
        .rposition(|e| matches!(e.event, Event::StepStarted { .. }));
    assert!(
        deleted
            .zip(retry)
            .is_some_and(|(deleted, retry)| deleted < retry)
    );
    let projection = Projection::from_events(&events);
    let assistant_texts: Vec<_> = projection
        .session
        .messages
        .iter()
        .filter(|row| row.role == hya_proto::Role::Assistant)
        .flat_map(|row| &row.parts)
        .filter_map(|part| match part {
            hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(assistant_texts, ["final answer"]);
    Ok(())
}

#[tokio::test]
async fn only_reasoning_or_length_requests_empty_turn_rescue() -> Result<(), CoreError> {
    for (first, expected) in [
        (Item::Reasoning, 2),
        (Item::BilledReasoning, 2),
        (Item::EmptyReasoning, 1),
        (Item::Text(""), 1),
    ] {
        let reasoning = matches!(first, Item::Reasoning);
        let (engine, agent, session, opens) =
            fixture(vec![vec![first], vec![Item::Text("rescued")]]).await;
        assert_eq!(
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await?,
            FinishReason::Stop
        );
        assert_eq!(*opens.lock().unwrap_or_else(|e| e.into_inner()), expected);
        let projection = engine.store().read_projection(session).await?;
        let notices = projection.session.messages.iter().flat_map(|m| &m.parts).filter(|p|
            matches!(p, hya_proto::PartProjection::Text { text, .. } if kernel::is_empty_turn_notice(text))).count();
        assert_eq!(notices, expected - 1);
        if reasoning {
            let mut parts = projection
                .session
                .messages
                .iter()
                .flat_map(|row| &row.parts);
            assert!(parts.clone().any(|part| matches!(part, hya_proto::PartProjection::Reasoning { text, provider_data: Some(data), .. } if text == "thinking" && data == &json!({"id": "prior-step"}))));
            assert!(parts.any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.ends_with("Where your previous thinking left off:\nthinking"))));
            let messages = kernel::request::projection_to_messages(
                &agent.name,
                &projection,
                &agent.model,
                &AttachmentData::new(),
            );
            let notice_id = projection
                .session
                .messages
                .iter()
                .find(|row| {
                    row.parts.iter().any(|p| {
                        matches!(p,
                hya_proto::PartProjection::Text {text, ..} if kernel::is_empty_turn_notice(text))
                    })
                })
                .unwrap()
                .id;
            assert!(messages.iter().all(|m| m.id() != notice_id));
        }
    }
    Ok(())
}

#[tokio::test]
async fn terminal_failure_after_retry_closes_once() {
    let (engine, agent, session, opens) = fixture(vec![
        vec![Item::Reasoning, Item::Error],
        vec![Item::Reasoning, Item::Fatal],
    ])
    .await;
    assert!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(*opens.lock().unwrap_or_else(|e| e.into_inner()), 2);
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
async fn retry_exhaustion_closes_once() -> Result<(), Box<dyn std::error::Error>> {
    let attempts = kernel::ROUND_RETRY_WAITS.len() + 1;
    let (engine, agent, session, opens) =
        fixture(vec![vec![Item::Reasoning, Item::Interrupted]; attempts]).await;
    let mut events = engine.bus().subscribe();
    let worker = tokio::spawn({
        let engine = engine.clone();
        async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        }
    });
    for wait in kernel::ROUND_RETRY_WAITS {
        while !matches!(events.recv().await?.event, Event::PartDeleted { .. }) {}
        tokio::time::pause();
        tokio::time::advance(wait + Duration::from_millis(1)).await;
        tokio::time::resume();
    }
    assert!(
        matches!(worker.await?, Err(CoreError::Provider(ref error)) if matches!(error.as_ref(), ProviderError::StreamInterrupted(_)))
    );
    assert_eq!(*opens.lock().unwrap_or_else(|e| e.into_inner()), attempts);
    let events = engine.store().replay(session).await?;
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
    Ok(())
}

#[tokio::test]
async fn cancellation_during_backoff_is_prompt() -> Result<(), Box<dyn std::error::Error>> {
    let (engine, agent, session, opens) = fixture(vec![
        vec![Item::Reasoning, Item::Error],
        vec![Item::Reasoning, Item::Error],
    ])
    .await;
    let cancel = CancellationToken::new();
    let mut events = engine.bus().subscribe();
    let task = {
        let e = engine.clone();
        let c = cancel.clone();
        tokio::spawn(async move { e.run_turn(session, &agent, c).await })
    };
    while !matches!(events.recv().await?.event, Event::PartDeleted { .. }) {}
    let started = Instant::now();
    cancel.cancel();
    assert!(matches!(
        task.await?,
        Ok(FinishReason::Cancelled) | Err(CoreError::Cancelled)
    ));
    assert_eq!(*opens.lock().unwrap_or_else(|e| e.into_inner()), 1);
    assert!(started.elapsed() < Duration::from_secs(1));
    Ok(())
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
