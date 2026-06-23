#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures::stream;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, Event, FinishReason, ModelRef, Role};
use yaca_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: yaca_proto::SessionId,
        message: yaca_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(stream::iter([Ok(Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
        })])))
    }
}

fn workdir() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "yaca-opencode-reference-guidance-{nanos}-{}",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

async fn state(workdir: &str, requests: Arc<Mutex<Vec<CompletionRequest>>>) -> AppState {
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
    std::fs::create_dir_all(format!("{workdir}/hidden")).unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider { requests })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "base system".to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn read_reference_state(workdir: &str, reference_dir: &str) -> AppState {
    std::fs::create_dir_all(workdir).unwrap();
    std::fs::create_dir_all(reference_dir).unwrap();
    std::fs::write(format!("{reference_dir}/guide.txt"), "reference body\n").unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "filePath": format!("{reference_dir}/guide.txt") }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "base system".to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = if body.is_null() {
        Body::empty()
    } else {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn create_session(app: axum::Router, workdir: &str) -> String {
    let (status, body) = request_json(
        app,
        Method::POST,
        "/sessions",
        json!({"agent": "build", "model": "fake", "workdir": workdir}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    format!("ses_{}", body["session"].as_str().unwrap().replace('-', ""))
}

#[tokio::test]
async fn opencode_prompt_system_includes_configured_reference_guidance() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let workdir = workdir();
    let app = router(state(&workdir, requests.clone()).await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": {
                    "path": "docs",
                    "description": "Project docs"
                },
                "hidden": {
                    "path": "hidden"
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session(app.clone(), &workdir).await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "read the docs"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let requests = requests.lock().unwrap();
    let system = requests[0].system.as_deref().unwrap();
    assert!(system.contains("base system"));
    assert!(system.contains("<available_references>"));
    assert!(system.contains("<name>docs</name>"));
    assert!(system.contains("<description>Project docs</description>"));
    assert!(!system.contains("<name>hidden</name>"));
}

#[tokio::test]
async fn opencode_reference_directories_allow_external_tool_reads() {
    let reference_dir = workdir();
    let workdir = workdir();
    let app = router(read_reference_state(&workdir, &reference_dir).await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": { "path": reference_dir }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session(app.clone(), &workdir).await;
    let (status, _message) = request_json(
        app.clone(),
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "read the reference"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, messages) = request_json(
        app,
        Method::GET,
        &format!("/session/{session}/message"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tool = messages[1]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "tool" && part["tool"] == "read")
        .unwrap();
    assert_eq!(tool["state"]["status"], "completed");
    assert!(
        tool["state"]["output"]
            .as_str()
            .unwrap()
            .contains("reference body")
    );
}
