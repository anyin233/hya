//! Focused server coverage for Workflow route parity and slash interception.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishReason, MemberId, MemberRunStatus, MessageId, ModelRef, OwnerRunId,
    SessionId, WorkflowAvailability, WorkflowCommand, WorkflowCommandResult, WorkflowDelivery,
    WorkflowIdentity, WorkflowMemberRole, WorkflowRevision, WorkflowRunId, WorkflowSourceId,
    WorkflowStagePlan,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_server::{
    AppState, WorkflowControl, WorkflowControlError, WorkflowControlFuture,
    WorkflowDecorationFuture, router,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};
use tower::ServiceExt;

struct CountingProvider {
    inner: FakeProvider,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for CountingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.stream(request, session, message).await
    }
}

struct BlockingProvider {
    inner: FakeProvider,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Provider for BlockingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.stream(request, session, message).await
    }
}

#[derive(Clone)]
struct BlockingControl {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl BlockingControl {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

impl WorkflowControl for BlockingControl {
    fn execute(
        &self,
        _session: SessionId,
        _command: WorkflowCommand,
        _delivery: WorkflowDelivery,
    ) -> WorkflowControlFuture<'_> {
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(WorkflowCommandResult::State {
                state: Default::default(),
            })
        })
    }
}

#[derive(Clone, Default)]
struct RecordingControl {
    calls: Arc<Mutex<Vec<(SessionId, WorkflowCommand, WorkflowDelivery)>>>,
}

impl WorkflowControl for RecordingControl {
    fn execute(
        &self,
        session: SessionId,
        command: WorkflowCommand,
        delivery: WorkflowDelivery,
    ) -> WorkflowControlFuture<'_> {
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            calls.lock().await.push((session, command, delivery));
            Ok(WorkflowCommandResult::State {
                state: Default::default(),
            })
        })
    }

    fn decorate(
        &self,
        _session: SessionId,
        mut state: hya_proto::WorkflowProjection,
    ) -> WorkflowDecorationFuture<'_> {
        Box::pin(async move {
            state.availability = state
                .selection
                .as_ref()
                .map(|_| WorkflowAvailability::Available);
            Ok(state)
        })
    }
}

struct RejectingControl;

impl WorkflowControl for RejectingControl {
    fn execute(
        &self,
        _session: SessionId,
        _command: WorkflowCommand,
        _delivery: WorkflowDelivery,
    ) -> WorkflowControlFuture<'_> {
        Box::pin(async {
            Err(WorkflowControlError::new(
                "WORKFLOW_BUSY",
                "another run is active",
            ))
        })
    }
}

async fn fixture_with_provider(
    provider: Arc<dyn Provider>,
) -> (AppState, SessionId, Arc<RecordingControl>) {
    let providers = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        permission,
        EventBus::default(),
    ));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    engine
        .record_workflow_event(
            session,
            Event::WorkflowSelected {
                session,
                workflow: WorkflowIdentity {
                    source: WorkflowSourceId::new("test:hydrated"),
                    name: "hydrated".to_string(),
                    revision: WorkflowRevision::from_bytes([7; 32]),
                },
            },
        )
        .await
        .unwrap();
    let member = MemberId::new();
    engine
        .store()
        .append_event(
            session,
            &Event::MemberSpawned {
                session,
                member,
                child: None,
                subagent_type: AgentName::new("general"),
                description: "Hydrated member work".to_string(),
                depth: 1,
                directive: "Do the hydrated work".to_string(),
                tool_call: None,
            },
        )
        .await
        .unwrap();
    engine
        .store()
        .append_event(
            session,
            &Event::MemberStatusChanged {
                session,
                member,
                status: MemberRunStatus::Running,
            },
        )
        .await
        .unwrap();
    let control = Arc::new(RecordingControl::default());
    let app = AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "test".to_string(),
            workdir: "/tmp".into(),
            reasoning: None,
        }),
    )
    .with_workflow_control(control.clone());
    (app, session, control)
}

async fn fixture() -> (AppState, SessionId, Arc<RecordingControl>, Arc<AtomicUsize>) {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let (app, session, control) = fixture_with_provider(Arc::new(CountingProvider {
        inner: FakeProvider::scripted(vec![]),
        calls: Arc::clone(&provider_calls),
    }))
    .await;
    (app, session, control, provider_calls)
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn compat_session_hydration_carries_runtime_workflow_availability() {
    let (app, session, _control, _provider_calls) = fixture().await;
    let response = router(app)
        .oneshot(
            Request::get(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["workflow"]["selection"]["name"], "hydrated");
    assert_eq!(body["workflow"]["availability"], "available");
    assert!(
        body.get("members").is_none(),
        "Session hydration must not expose the complete canonical member vector"
    );
    assert!(
        body.get("workflowActivity").is_none(),
        "selection without a run has no Workflow-scoped active member rows"
    );
}

#[tokio::test]
async fn compat_session_hydration_carries_only_bounded_workflow_activity() {
    let (app, session, _control, _provider_calls) = fixture().await;
    let projection = app.engine.read_projection(session).await.unwrap();
    let member = projection.session.members[0].member;
    let workflow = WorkflowIdentity {
        source: WorkflowSourceId::new("test:hydrated"),
        name: "hydrated".to_string(),
        revision: WorkflowRevision::from_bytes([7; 32]),
    };
    let run = WorkflowRunId::new();
    for event in [
        Event::WorkflowRunStarted {
            session,
            run,
            workflow,
            request_hash: "hash".to_string(),
            owner: OwnerRunId::new(),
            stages: vec![WorkflowStagePlan {
                id: "work".to_string(),
                title: Some("Hydrated work".to_string()),
                agent: AgentName::new("general"),
                mode: "once".to_string(),
                level: 0,
            }],
        },
        Event::WorkflowStageStarted {
            session,
            run,
            stage: "work".to_string(),
        },
        Event::WorkflowStageMemberLinked {
            session,
            run,
            stage: "work".to_string(),
            member,
            role: WorkflowMemberRole::Worker,
            iteration: 0,
        },
    ] {
        app.engine
            .record_workflow_event(session, event)
            .await
            .unwrap();
    }

    let response = router(app)
        .oneshot(
            Request::get(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(body.get("members").is_none());
    assert_eq!(body["workflowActivity"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        body["workflowActivity"][0]["member"],
        body["workflow"]["run"]["stages"][0]["members"][0]["member"]
    );
    assert_eq!(body["workflowActivity"][0]["status"], "running");
    assert_eq!(body["workflowActivity"][0]["work"], "Hydrated member work");
    assert!(body["workflowActivity"][0].get("directive").is_none());
}

#[tokio::test]
async fn typed_workflow_state_is_available_on_all_route_families() {
    let (app, session, control, provider_calls) = fixture().await;
    for path in [
        format!("/sessions/{session}/workflow"),
        format!("/session/{session}/workflow"),
        format!("/api/session/{session}/workflow"),
    ] {
        let response = router(app.clone())
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "state");
        assert!(body["state"].is_object());
    }
    let calls = control.calls.lock().await;
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|(called_session, _, delivery)| *called_session == session
                && *delivery == WorkflowDelivery::Started)
    );
    assert_eq!(provider_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn workflow_slash_command_bypasses_parent_model_on_all_command_routes() {
    let (app, session, control, provider_calls) = fixture().await;
    let commands = [
        "list",
        "info demo",
        "use demo",
        "run demo request=value",
        "state",
    ];
    for path in [
        format!("/sessions/{session}/command"),
        format!("/session/{session}/command"),
        format!("/api/session/{session}/command"),
    ] {
        for arguments in commands {
            let response = router(app.clone())
                .oneshot(
                    Request::post(&path)
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"command": "workflow", "arguments": arguments}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                provider_calls.load(Ordering::Relaxed),
                0,
                "Workflow slash commands must never admit the parent model"
            );
            assert!(body_json(response).await["kind"].is_string());
        }
    }
    let calls = control.calls.lock().await;
    assert_eq!(calls.len(), 15);
    assert!(
        calls
            .iter()
            .all(|(_, _, delivery)| *delivery == WorkflowDelivery::Started)
    );
    assert_eq!(provider_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn typed_workflow_commands_use_all_route_families() {
    let (app, session, control, provider_calls) = fixture().await;
    for path in [
        format!("/sessions/{session}/workflow"),
        format!("/session/{session}/workflow"),
        format!("/api/session/{session}/workflow"),
    ] {
        let response = router(app.clone())
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"command": "list"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let calls = control.calls.lock().await;
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|(_, command, delivery)| {
        matches!(command, WorkflowCommand::List) && *delivery == WorkflowDelivery::Started
    }));
    assert_eq!(provider_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn typed_workflow_error_preserves_status_code_and_message() {
    let (app, session, _control, _provider_calls) = fixture().await;
    let response = router(app.with_workflow_control(Arc::new(RejectingControl)))
        .oneshot(
            Request::post(format!("/session/{session}/workflow"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"command": "state"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "WORKFLOW_BUSY");
    assert_eq!(body["error"]["message"], "another run is active");
}

#[tokio::test]
async fn malformed_workflow_slash_returns_structured_syntax_error() {
    let (app, session, _control, _provider_calls) = fixture().await;
    let response = router(app)
        .oneshot(
            Request::post(format!("/sessions/{session}/command"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"command": "workflow", "arguments": "list extra"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "WORKFLOW_SYNTAX");
    assert!(body["error"]["message"].as_str().is_some());
}

#[tokio::test]
async fn workflow_mutation_reservation_blocks_parent_model_admission() {
    let (app, session, _recording, provider_calls) = fixture().await;
    let control = BlockingControl::new();
    let entered = Arc::clone(&control.entered);
    let release = Arc::clone(&control.release);
    let app = router(app.with_workflow_control(Arc::new(control)));

    let workflow_app = app.clone();
    let workflow = tokio::spawn(async move {
        workflow_app
            .oneshot(
                Request::post(format!("/sessions/{session}/workflow"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"command": "select", "name": "demo"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    entered.notified().await;

    let parent = app
        .clone()
        .oneshot(
            Request::post(format!("/sessions/{session}/prompt"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "parent"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(parent.status(), StatusCode::CONFLICT);
    assert_eq!(provider_calls.load(Ordering::Relaxed), 0);

    release.notify_one();
    let workflow_response = workflow.await.unwrap();
    assert_eq!(workflow_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn workflow_mutation_rejects_active_parent_but_state_remains_readable() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(BlockingProvider {
        inner: FakeProvider::scripted(vec![FakeStep::Finish(FinishReason::Stop)]),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let (app, session, control) = fixture_with_provider(provider).await;
    let app = router(app);

    let parent_app = app.clone();
    let parent = tokio::spawn(async move {
        parent_app
            .oneshot(
                Request::post(format!("/sessions/{session}/prompt"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"text": "parent"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    entered.notified().await;

    let mutation = app
        .clone()
        .oneshot(
            Request::post(format!("/sessions/{session}/workflow"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"command": "select", "name": "demo"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mutation.status(), StatusCode::CONFLICT);

    let messages_before = body_json(
        app.clone()
            .oneshot(
                Request::get(format!("/session/{session}/message"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let slash = app
        .clone()
        .oneshot(
            Request::post(format!("/sessions/{session}/command"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"command": "workflow", "arguments": "use demo"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(slash.status(), StatusCode::CONFLICT);
    let messages_after = body_json(
        app.clone()
            .oneshot(
                Request::get(format!("/session/{session}/message"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        messages_after, messages_before,
        "busy slash mutation must not append transcript rows"
    );

    let state = app
        .clone()
        .oneshot(
            Request::get(format!("/sessions/{session}/workflow"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(state.status(), StatusCode::OK);
    assert_eq!(body_json(state).await["kind"], "state");

    release.notify_one();
    let parent_response = parent.await.unwrap();
    assert_eq!(parent_response.status(), StatusCode::OK);

    let calls = control.calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert!(matches!(calls[0].1, WorkflowCommand::State));
}
