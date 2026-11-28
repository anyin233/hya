//! Shared HTTP, gRPC, SSE, and provider helpers for `hya-server` integration tests.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use base64::Engine as _;
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_engine::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, Provider, ProviderError,
    ProviderRouter,
};
use hya_server::{AppState, Server, build};
use hya_store::SessionStore;
use hya_tool::permission::AskRequest;
use hya_tool::{PermissionPlane, PermissionRules};
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::task::JoinHandle;
use tower::ServiceExt;

use crate::support;

pub fn default_caps() -> Capabilities {
    Capabilities {
        streaming_tool_calls: true,
        usage_reporting: true,
        max_context: 200_000,
        ..Capabilities::default()
    }
}

pub fn text_events(session: SessionId, message: MessageId, text: &str) -> Vec<Event> {
    let part = PartId::new();
    vec![
        Event::TextStart {
            session,
            message,
            part,
        },
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_owned(),
        },
        Event::TextEnd {
            session,
            message,
            part,
        },
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ]
}

/// Instant provider returning one short text part per round.
pub struct TextProvider;

#[async_trait]
impl Provider for TextProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(default_caps())
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(futures::stream::iter(
            text_events(s, m, "done").into_iter().map(Ok),
        )))
    }
}

/// Holds every completion round open until `gate` receives a permit.
pub struct GatedProvider {
    pub gate: Arc<Semaphore>,
    pub entered: Option<Arc<Semaphore>>,
    pub entered_notify: Option<Arc<Notify>>,
}

impl GatedProvider {
    pub fn new(gate: Arc<Semaphore>) -> Self {
        Self {
            gate,
            entered: None,
            entered_notify: None,
        }
    }
    pub fn with_entered(gate: Arc<Semaphore>, entered: Arc<Semaphore>) -> Self {
        Self {
            gate,
            entered: Some(entered),
            entered_notify: None,
        }
    }
    pub fn with_notify(gate: Arc<Semaphore>, entered_notify: Arc<Notify>) -> Self {
        Self {
            gate,
            entered: None,
            entered_notify: Some(entered_notify),
        }
    }
}

#[async_trait]
impl Provider for GatedProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(default_caps())
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if let Some(entered) = &self.entered {
            entered.add_permits(1);
        }
        if let Some(notify) = &self.entered_notify {
            notify.notify_waiters();
        }
        let gate = Arc::clone(&self.gate);
        let events = text_events(s, m, "done");
        let stream = futures::stream::once(async move {
            gate.acquire().await.unwrap().forget();
        })
        .flat_map(move |()| futures::stream::iter(events.clone().into_iter().map(Ok)));
        Ok(Box::pin(stream))
    }
}

pub fn default_agent(workdir: PathBuf) -> Arc<AgentSpec> {
    Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".into(),
        workdir,
        reasoning: None,
    })
}

pub fn engine_on<P: Provider + 'static>(
    store: SessionStore,
    provider: P,
    rules: PermissionRules,
) -> (Arc<SessionEngine>, mpsc::UnboundedReceiver<AskRequest>) {
    let (perm, asks) = PermissionPlane::new(rules);
    let engine = Arc::new(
        SessionEngine::new(
            store,
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_legacy_coordination(),
    );
    (engine, asks)
}

pub fn state_of(engine: Arc<SessionEngine>, workdir: impl Into<PathBuf>) -> AppState {
    AppState::new(engine, default_agent(workdir.into()))
}

pub async fn state_with_rules<P: Provider + 'static>(
    provider: P,
    rules: PermissionRules,
    workdir: PathBuf,
) -> (
    AppState,
    mpsc::UnboundedReceiver<AskRequest>,
    Arc<SessionEngine>,
) {
    let store = SessionStore::connect_memory().await.unwrap();
    let (engine, asks) = engine_on(store, provider, rules);
    (state_of(Arc::clone(&engine), workdir), asks, engine)
}

pub async fn state_with_provider<P: Provider + 'static>(provider: P) -> AppState {
    state_with_rules(provider, PermissionRules::default(), std::env::temp_dir())
        .await
        .0
}

pub async fn base_state() -> AppState {
    state_with_provider(FakeProvider::scripted(vec![])).await
}

pub async fn send_raw(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let resp = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

pub async fn send(app: &Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    send_raw(app, method, uri, &[], body).await
}

pub async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    send(app, Method::GET, uri, Value::Null).await
}
pub async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    send(app, Method::POST, uri, body).await
}
pub async fn patch(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    send(app, Method::PATCH, uri, body).await
}
pub async fn put(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    send(app, Method::PUT, uri, body).await
}
pub async fn del(app: &Router, uri: &str) -> (StatusCode, Value) {
    send(app, Method::DELETE, uri, Value::Null).await
}

pub async fn ok_get(app: &Router, uri: &str) -> Value {
    expect_ok(get_json(app, uri).await)
}
pub async fn ok_post(app: &Router, uri: &str, body: Value) -> Value {
    expect_ok(post(app, uri, body).await)
}
pub async fn ok_patch(app: &Router, uri: &str, body: Value) -> Value {
    expect_ok(patch(app, uri, body).await)
}
pub async fn ok_put(app: &Router, uri: &str, body: Value) -> Value {
    expect_ok(put(app, uri, body).await)
}
pub async fn ok_del(app: &Router, uri: &str) -> Value {
    expect_ok(del(app, uri).await)
}

pub fn expect_ok((status, body): (StatusCode, Value)) -> Value {
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

pub fn assert_error(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "{body}");
    assert_eq!(body["error"]["code"], json!(code), "{body}");
}

pub fn assert_err((status, body): (StatusCode, Value), expected: StatusCode, code: &str) {
    assert_error(status, &body, expected, code);
}

pub fn assert_bad(resp: (StatusCode, Value)) {
    assert_err(resp, StatusCode::BAD_REQUEST, "invalid_argument");
}

pub fn id_str(v: &Value) -> String {
    v.as_str().unwrap().to_owned()
}

pub fn grpc_json<T: serde::Serialize>(res: Result<tonic::Response<T>, tonic::Status>) -> Value {
    serde_json::to_value(res.unwrap().into_inner()).unwrap()
}

pub fn grpc_code<T: std::fmt::Debug>(
    res: Result<tonic::Response<T>, tonic::Status>,
) -> tonic::Code {
    res.unwrap_err().code()
}

pub async fn create_session(app: &Router, workdir: &Path) -> String {
    create_session_id(app, json!({ "workdir": workdir.to_string_lossy() })).await
}

pub async fn create_session_with(app: &Router, extra: Value) -> (StatusCode, Value) {
    let mut body = json!({ "agent": "build", "model": "fake" });
    if let Some(map) = extra.as_object() {
        for (key, value) in map {
            body[key] = value.clone();
        }
    }
    post(app, "/v2/sessions", body).await
}

pub async fn create_session_id(app: &Router, extra: Value) -> String {
    id_str(&expect_ok(create_session_with(app, extra).await)["session"]["id"])
}

pub async fn start_turn(app: &Router, session: &str, body: Value) -> String {
    id_str(&ok_post(app, &format!("/v2/sessions/{session}/turns"), body).await["turn"]["id"])
}

pub async fn wait_turn(app: &Router, session: &str, turn: &str) -> Value {
    ok_post(
        app,
        &format!("/v2/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await
}

pub async fn wait_not_busy(app: &Router, session: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if get_json(app, &format!("/v2/sessions/{session}")).await.1["busy"] != json!(true) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("session {session} stayed busy");
}

pub async fn run_prompt(app: &Router, session: &str, text: &str) -> String {
    let turn = start_turn(app, session, json!({"prompt": {"text": text}})).await;
    let waited = wait_turn(app, session, &turn).await;
    assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");
    wait_not_busy(app, session).await;
    turn
}

/// Subscribe to an SSE route synchronously before returning its body stream.
pub async fn open_sse(app: &Router, uri: &str) -> axum::body::BodyDataStream {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    resp.into_body().into_data_stream()
}

pub async fn sse_frames_until_stream(
    mut stream: axum::body::BodyDataStream,
    stop: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let mut frames = Vec::new();
    let mut buffer = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(item) =
            tokio::time::timeout(remaining.min(Duration::from_millis(500)), stream.next()).await
        else {
            continue;
        };
        let Some(Ok(bytes)) = item else { break };
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end) = buffer.find("\n\n") {
            let record: String = buffer.drain(..end + 2).collect();
            for line in record.lines() {
                if let Some(data) = line.strip_prefix("data:")
                    && let Ok(frame) = serde_json::from_str::<Value>(data.trim())
                {
                    let finished = stop(&frame);
                    frames.push(frame);
                    if finished {
                        return frames;
                    }
                }
            }
        }
    }
    frames
}

pub async fn collect_sse(
    app: &Router,
    uri: &str,
    stop: impl Fn(&Value) -> bool + Send + 'static,
) -> JoinHandle<Vec<Value>> {
    let stream = open_sse(app, uri).await;
    tokio::spawn(async move { sse_frames_until_stream(stream, stop).await })
}

pub async fn serve_server(
    server: &Server,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> (SocketAddr, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = server.clone();
    let task = tokio::spawn(async move { server.serve(listener, stop).await });
    (addr, task)
}

pub async fn grpc_channel(state: AppState) -> tonic::transport::Channel {
    let server = build(state);
    let (addr, _task) = serve_server(&server, std::future::pending()).await;
    tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

pub fn enc(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
pub fn b64_decode(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap_or_default()
}
