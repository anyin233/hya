//! Automatic session titles: the first prompt turn of a root session titles
//! it in the background with the fixed `title` agent (one `SessionTitled`,
//! streamed as `sessionUpdated.title`, billed as `purpose: title`); child
//! sessions, manually titled sessions, later turns, and restarts get none,
//! and a failing title call is ignored.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::{StreamExt, stream};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, SessionId, TokenUsage,
    UsagePurpose,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Opening of the fixed `title` agent's prompt (`core-agents` preset,
/// `prompts/title.md`), which the session's bound catalog resolves.
const TITLE_PROMPT: &str = "You are a title generator.";

/// One recorded title request: title system prompt, user texts,
/// temperature, max tokens, model.
type TitleRequest = (
    Option<String>,
    Vec<String>,
    Option<f32>,
    Option<u32>,
    String,
);

/// Answers the title agent with `title` (or fails when `title` is `None`)
/// and every other request with a plain reply; records title requests.
struct TitlingProvider {
    title: Option<String>,
    title_requests: Arc<Mutex<Vec<TitleRequest>>>,
}

#[async_trait]
impl Provider for TitlingProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let script = if req
            .system
            .as_deref()
            .is_some_and(|system| system.contains(TITLE_PROMPT))
        {
            let texts = req
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::User { parts, .. } => Some(
                        parts
                            .iter()
                            .filter_map(|part| match part {
                                Part::Text { text, .. } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect();
            self.title_requests.lock().unwrap().push((
                Some(TITLE_PROMPT.to_string()),
                texts,
                req.temperature,
                req.max_output_tokens,
                req.model.to_string(),
            ));
            let Some(title) = &self.title else {
                return Err(ProviderError::Incompatible("title model down".to_string()));
            };
            vec![
                FakeStep::Text(format!("{title}\n")),
                FakeStep::Usage(TokenUsage {
                    input: 40,
                    output: 6,
                    ..TokenUsage::default()
                }),
                FakeStep::Finish(FinishReason::Stop),
            ]
        } else {
            vec![
                FakeStep::Text("ok".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        };
        let events = FakeProvider::materialize(&script, session, message);
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

struct Fixture {
    app: axum::Router,
    engine: Arc<SessionEngine>,
    title_requests: Arc<Mutex<Vec<TitleRequest>>>,
    dir: PathBuf,
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-v1-title-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(store: SessionStore, title: Option<&str>, dir: PathBuf) -> Fixture {
    let title_requests = Arc::new(Mutex::new(Vec::new()));
    let provider = TitlingProvider {
        title: title.map(str::to_owned),
        title_requests: Arc::clone(&title_requests),
    };
    let (perm, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        store,
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ));
    let state = AppState::new(
        Arc::clone(&engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: dir.clone(),
            reasoning: None,
        }),
    )
    .with_auto_title(true);
    Fixture {
        app: router(state),
        engine,
        title_requests,
        dir,
    }
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create_session(fx: &Fixture, extra: Value) -> String {
    let mut body = json!({"agent": "build", "model": "fake", "workdir": fx.dir.to_string_lossy()});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    let (status, created) = call(&fx.app, Method::POST, "/v1/sessions", body).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created["session"]["id"].as_str().unwrap().to_owned()
}

async fn run_turn(fx: &Fixture, session: &str, text: &str) {
    let (status, created) = call(
        &fx.app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": text}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let turn = created["turn"]["id"].as_str().unwrap().to_owned();
    let (status, waited) = call(
        &fx.app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");
}

async fn title_of(fx: &Fixture, session: &str) -> Value {
    let (status, info) = call(
        &fx.app,
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    info["title"].clone()
}

async fn wait_title(fx: &Fixture, session: &str) -> Value {
    for _ in 0..200 {
        let title = title_of(fx, session).await;
        if !title.is_null() {
            return title;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("session {session} was never titled");
}

/// Give a background title task that should not exist time to show itself.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

fn titled_events(events: &[hya_proto::Envelope]) -> Vec<String> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::SessionTitled { title, .. } => Some(title.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_prompt_titles_a_root_session_in_the_background() {
    let fx = fixture(
        SessionStore::connect_memory().await.unwrap(),
        Some("Fix the login bug"),
        tempdir(),
    );
    let session = create_session(&fx, json!({})).await;

    // The title streams as `sessionUpdated.title`.
    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/sessions/{session}/events/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut frames = resp.into_body().into_data_stream();
    let streamed = tokio::spawn(async move {
        let mut buffer = String::new();
        while let Some(Ok(bytes)) = frames.next().await {
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            for line in buffer.lines() {
                if let Some(data) = line.strip_prefix("data:")
                    && let Ok(frame) = serde_json::from_str::<Value>(data.trim())
                    && let Some(title) = frame["event"]["sessionUpdated"]["title"].as_str()
                {
                    return title.to_owned();
                }
            }
        }
        panic!("stream ended without a title");
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    run_turn(
        &fx,
        &session,
        "please fix the login bug in auth.rs\nit 500s",
    )
    .await;
    assert_eq!(wait_title(&fx, &session).await, json!("Fix the login bug"));
    let streamed = tokio::time::timeout(Duration::from_secs(5), streamed)
        .await
        .expect("sessionUpdated.title frame")
        .unwrap();
    assert_eq!(streamed, "Fix the login bug");

    // One request to the fixed title agent, carrying the prompt.
    let requests = fx.title_requests.lock().unwrap().clone();
    assert_eq!(
        requests,
        vec![(
            Some(TITLE_PROMPT.to_string()),
            vec!["please fix the login bug in auth.rs\nit 500s".to_string()],
            Some(0.0),
            Some(128),
            "fake".to_string(),
        )]
    );

    // Recorded once, and billed to the session as `purpose: title`.
    let parsed: SessionId = session.parse().unwrap();
    let events = fx.engine.replay(parsed).await.unwrap();
    assert_eq!(titled_events(&events), vec!["Fix the login bug"]);
    assert!(events.iter().any(|envelope| matches!(
        &envelope.event,
        Event::UsageRecorded {
            purpose: UsagePurpose::Title,
            message: None,
            ..
        }
    )));
    let _ = std::fs::remove_dir_all(&fx.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn later_turns_and_a_restart_never_retitle() {
    let store = SessionStore::connect_memory().await.unwrap();
    let dir = tempdir();
    let fx = fixture(store.clone(), Some("Plan the release"), dir.clone());
    let session = create_session(&fx, json!({})).await;
    run_turn(&fx, &session, "plan the release").await;
    assert_eq!(wait_title(&fx, &session).await, json!("Plan the release"));
    run_turn(&fx, &session, "and the changelog").await;
    settle().await;
    assert_eq!(fx.title_requests.lock().unwrap().len(), 1);

    // A new process over the same database.
    let restarted = fixture(store, Some("Something else"), dir);
    run_turn(&restarted, &session, "one more").await;
    settle().await;
    assert!(restarted.title_requests.lock().unwrap().is_empty());
    let parsed: SessionId = session.parse().unwrap();
    let events = restarted.engine.replay(parsed).await.unwrap();
    assert_eq!(titled_events(&events), vec!["Plan the release"]);
    let _ = std::fs::remove_dir_all(&restarted.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_titles_and_child_sessions_are_not_titled() {
    let fx = fixture(
        SessionStore::connect_memory().await.unwrap(),
        Some("Generated"),
        tempdir(),
    );
    let named = create_session(&fx, json!({"title": "My own name"})).await;
    run_turn(&fx, &named, "do things").await;

    let root = create_session(&fx, json!({})).await;
    let (status, renamed) = call(
        &fx.app,
        Method::PATCH,
        &format!("/v1/sessions/{root}"),
        json!({"title": "Renamed first"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    run_turn(&fx, &root, "do things").await;

    let child = create_session(&fx, json!({"parent": root})).await;
    run_turn(&fx, &child, "subagent work").await;
    settle().await;

    assert!(fx.title_requests.lock().unwrap().is_empty());
    assert_eq!(title_of(&fx, &named).await, json!("My own name"));
    assert_eq!(title_of(&fx, &root).await, json!("Renamed first"));
    assert!(title_of(&fx, &child).await.is_null());
    let _ = std::fs::remove_dir_all(&fx.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_title_call_leaves_the_turn_and_session_alone() {
    let fx = fixture(
        SessionStore::connect_memory().await.unwrap(),
        None,
        tempdir(),
    );
    let session = create_session(&fx, json!({})).await;
    run_turn(&fx, &session, "hello there").await;
    settle().await;
    assert_eq!(fx.title_requests.lock().unwrap().len(), 1);
    assert!(title_of(&fx, &session).await.is_null());
    let _ = std::fs::remove_dir_all(&fx.dir);
}
