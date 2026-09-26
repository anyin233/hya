//! One server, one state, both protocols: `hya_server::build` serves the
//! `/v1` HTTP router and the `hya.v1` gRPC services on the same listener
//! (routed by `content-type: application/grpc*`), from one server state whose
//! background drivers start once; a second listener serving the same
//! `Server` (`HYA_GRPC_BIND`) shares it; shutdown sends `serverStopping` to
//! SSE and gRPC streams alike; the Host guard checks gRPC `:authority`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, Full};
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, HostPolicy, ShutdownReason, V1Grpc, build, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

/// Answers every round with one text part once the test adds a permit to
/// `gate`, so a turn stays running until released.
struct GatedProvider {
    gate: Arc<Semaphore>,
}

#[async_trait]
impl Provider for GatedProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let gate = Arc::clone(&self.gate);
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "done".to_string(),
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
        ];
        let stream = futures::stream::once(async move {
            gate.acquire().await.unwrap().forget();
        })
        .flat_map(move |()| futures::stream::iter(events.clone().into_iter().map(Ok)));
        Ok(Box::pin(stream))
    }
}

async fn state(gate: Arc<Semaphore>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(GatedProvider { gate })));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
    .with_scratch_root(support::tempdir("one-server-scratch"))
}

/// Serve `server` on a fresh loopback listener until `stop` fires.
async fn serve(
    server: &hya_server::Server,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = server.clone();
    let task = tokio::spawn(async move { server.serve(listener, stop).await });
    (addr, task)
}

type Sender = hyper::client::conn::http1::SendRequest<Full<Bytes>>;

/// An HTTP/1.1 connection to `addr`.
async fn http(addr: SocketAddr) -> Sender {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);
    sender
}

fn request(
    addr: SocketAddr,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> hyper::Request<Full<Bytes>> {
    hyper::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", addr.to_string())
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(
            body.map(|body| body.to_string()).unwrap_or_default(),
        )))
        .unwrap()
}

async fn call(addr: SocketAddr, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
    let mut sender = http(addr).await;
    let response = sender
        .send_request(request(addr, method, uri, body))
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// An h2c (prior knowledge) gRPC channel to `addr`.
async fn channel(addr: SocketAddr) -> tonic::transport::Channel {
    tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

async fn create_session(addr: SocketAddr, project: Option<&str>) -> String {
    let mut body = json!({"agent": "build", "model": "fake", "workdir": std::env::temp_dir().to_string_lossy()});
    if let Some(project) = project {
        body["projectId"] = json!(project);
    }
    let (status, body) = call(addr, "POST", "/v1/sessions", Some(body)).await;
    assert_eq!(status, 200, "{body}");
    body["session"]["id"].as_str().unwrap().to_owned()
}

/// Every `data:` frame of an SSE body until it ends (at most `wait`).
async fn sse_frames_to_end(body: &mut hyper::body::Incoming, wait: Duration) -> Vec<Value> {
    let mut text = String::new();
    let ended = tokio::time::timeout(wait, async {
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else { break };
            if let Some(data) = frame.data_ref() {
                text.push_str(&String::from_utf8_lossy(data));
            }
        }
    })
    .await
    .is_ok();
    assert!(ended, "the SSE stream did not end: {text}");
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect()
}

/// `data:` frames of an SSE body read for `wait`.
async fn sse_frames_for(body: &mut hyper::body::Incoming, wait: Duration) -> Vec<Value> {
    let mut text = String::new();
    let _ = tokio::time::timeout(wait, async {
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else { break };
            if let Some(data) = frame.data_ref() {
                text.push_str(&String::from_utf8_lossy(data));
            }
        }
    })
    .await;
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect()
}

async fn open_sse(addr: SocketAddr, uri: &str) -> hyper::body::Incoming {
    let mut sender = http(addr).await;
    let response = sender
        .send_request(request(addr, "GET", uri, None))
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{uri}");
    response.into_body()
}

#[tokio::test]
async fn http_and_grpc_answer_on_one_port_over_one_state() {
    let app = state(Arc::new(Semaphore::new(0))).await;
    let server = build(app);
    let (addr, _task) = serve(&server, std::future::pending()).await;

    let (status, health) = call(addr, "GET", "/v1/health", None).await;
    assert_eq!(status, 200, "{health}");
    assert_eq!(health["ok"], json!(true));

    let channel = channel(addr).await;
    let sessions_channel = channel.clone();
    let health = pb::process_client::ProcessClient::new(channel.clone())
        .get_health(pb::GetHealthRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert!(health.ok);

    // A session created over HTTP is visible over gRPC on the same port,
    // and the other way around.
    let session = create_session(addr, None).await;
    let mut sessions = pb::session_client::SessionClient::new(channel);
    let got = sessions
        .get_session(pb::GetSessionRequest {
            session: session.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.id, session);
    let created = sessions
        .create_session(pb::CreateSessionRequest {
            agent: "build".to_owned(),
            model: "fake".to_owned(),
            workdir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let id = created.session.unwrap().id;
    let (status, body) = call(addr, "GET", &format!("/v1/sessions/{id}"), None).await;
    assert_eq!(status, 200, "{body}");

    // A gRPC loopback caller may use the loopback-only rpcs.
    let status = pb::relay_control_client::RelayControlClient::new(sessions_channel)
        .get_relay_status(pb::GetRelayStatusRequest::default())
        .await;
    assert!(status.is_ok(), "{status:?}");
}

/// Building a router, a gRPC binding, and a combined server from clones of
/// one `AppState` makes one server state: the background drivers run once,
/// so a busy change publishes one `projectsUpdated`, not one per driver.
#[tokio::test]
async fn one_state_runs_the_background_drivers_once() {
    let gate = Arc::new(Semaphore::new(0));
    let app = state(Arc::clone(&gate)).await;
    let _router = router(app.clone());
    let _grpc = V1Grpc::new(app.clone());
    let server = build(app.clone());
    let _again = router(app.clone());
    assert_eq!(app.server_builds(), 1);

    let (addr, _task) = serve(&server, std::future::pending()).await;
    let (status, project) = call(
        addr,
        "POST",
        "/v1/projects",
        Some(json!({"name": "p", "roots": [std::env::temp_dir().to_string_lossy()]})),
    )
    .await;
    assert_eq!(status, 200, "{project}");
    let project = project["id"].as_str().unwrap().to_owned();
    let session = create_session(addr, Some(&project)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut global = open_sse(addr, "/v1/events/stream?interactionsOnly=true").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = call(
        addr,
        "POST",
        &format!("/v1/sessions/{session}/turns"),
        Some(json!({ "prompt": { "text": "work" } })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    let busy = sse_frames_for(&mut global, Duration::from_millis(800)).await;
    let notices = |frames: &[Value]| {
        frames
            .iter()
            .filter(|frame| frame["event"]["projectsUpdated"].is_object())
            .count()
    };
    assert_eq!(notices(&busy), 1, "busy: {busy:#?}");

    gate.add_permits(1);
    let (status, body) = call(
        addr,
        "POST",
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let idle = sse_frames_for(&mut global, Duration::from_millis(800)).await;
    assert_eq!(notices(&idle), 1, "idle: {idle:#?}");

    // A reconfigured state is a new server.
    let other = app.clone().with_auto_title(false);
    let _other = router(other.clone());
    assert_eq!(other.server_builds(), 2);
}

/// The shutdown closes SSE and gRPC streams of the one listener alike,
/// each with `serverStopping` last, and the server then finishes.
#[tokio::test]
async fn server_stopping_reaches_sse_and_grpc_streams() {
    let app = state(Arc::new(Semaphore::new(0))).await;
    let streams = app.streams();
    let server = build(app);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let (addr, task) = serve(&server, async move {
        let _ = stop_rx.await;
        streams.close(ShutdownReason::Restart);
    })
    .await;
    let session = create_session(addr, None).await;
    let mut sse = open_sse(addr, "/v1/events/stream").await;
    let mut events = pb::events_client::EventsClient::new(channel(addr).await);
    let global = events
        .stream_global_events(pb::StreamGlobalEventsRequest::default())
        .await
        .unwrap()
        .into_inner();
    let own = events
        .stream_session_events(pb::StreamSessionEventsRequest {
            session,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    tokio::time::sleep(Duration::from_millis(150)).await;
    stop_tx.send(()).unwrap();

    let frames = sse_frames_to_end(&mut sse, Duration::from_secs(5)).await;
    assert_eq!(
        frames.last().unwrap()["event"]["serverStopping"],
        json!({"reason": "restart"}),
        "{frames:#?}"
    );
    for (label, mut stream) in [("global", global), ("session", own)] {
        let mut last = None;
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = stream.next().await {
                let Ok(frame) = frame else { break };
                if let Some(pb::stream_frame::Frame::Event(event)) = frame.frame {
                    last = event.payload;
                }
            }
        })
        .await
        .is_ok();
        assert!(ended, "{label}: the gRPC stream must end");
        match last {
            Some(pb::stream_event::Payload::ServerStopping(stopping)) => {
                assert_eq!(stopping.reason, "restart", "{label}");
            }
            other => panic!("{label}: last payload {other:?}"),
        }
    }
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the server finishes its graceful shutdown")
        .unwrap();
}

/// A second listener serving the same `Server` (the `HYA_GRPC_BIND` extra
/// listener) shares its state: a session and a PTY made over HTTP on one
/// are visible over gRPC on the other at once, and busy agrees.
#[tokio::test]
async fn an_extra_listener_shares_the_state() {
    let gate = Arc::new(Semaphore::new(0));
    let app = state(Arc::clone(&gate)).await;
    let server = build(app.clone());
    let (main, _main) = serve(&server, std::future::pending()).await;
    let (extra, _extra) = serve(&server, std::future::pending()).await;
    assert_eq!(app.server_builds(), 1);

    let (status, project) = call(
        main,
        "POST",
        "/v1/projects",
        Some(json!({"name": "p", "roots": [std::env::temp_dir().to_string_lossy()]})),
    )
    .await;
    assert_eq!(status, 200, "{project}");
    let project = project["id"].as_str().unwrap().to_owned();
    let session = create_session(main, Some(&project)).await;
    let channel = channel(extra).await;
    let got = pb::session_client::SessionClient::new(channel.clone())
        .get_session(pb::GetSessionRequest {
            session: session.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.id, session);

    // Process-local state (the PTY table) is the same state.
    let dir = support::tempdir("one-server-pty");
    let (status, pty) = call(
        main,
        "POST",
        "/v1/pty",
        Some(json!({"shell": "/bin/sh", "cwd": dir.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, 200, "{pty}");
    let id = pty["id"].as_str().unwrap().to_owned();
    let got = pb::pty_client::PtyClient::new(channel.clone())
        .get_pty(pb::GetPtyRequest { id: id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.id, id);

    // Busy agrees across listeners and protocols.
    let (status, body) = call(
        main,
        "POST",
        &format!("/v1/sessions/{session}/turns"),
        Some(json!({ "prompt": { "text": "work" } })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let mut projects = pb::project_client::ProjectClient::new(channel);
    let row = projects
        .get_project(pb::GetProjectRequest {
            project: project.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(row.busy, "{row:?}");
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let row = projects
                .get_project(pb::GetProjectRequest {
                    project: project.clone(),
                })
                .await
                .unwrap()
                .into_inner();
            if !row.busy {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("busy clears on the extra listener too");
}

/// The Host guard checks gRPC `:authority` on the shared port like the
/// HTTP `Host`: a foreign name is refused, `--allow-host` names pass.
#[tokio::test]
async fn the_host_guard_checks_grpc_authority() {
    let app = state(Arc::new(Semaphore::new(0)))
        .await
        .with_allowed_hosts(HostPolicy::with_hosts(["hya.example.lan"]).unwrap());
    let server = build(app);
    let (addr, _task) = serve(&server, std::future::pending()).await;
    let health = |origin: &'static str| async move {
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .origin(origin.parse().unwrap())
            .connect()
            .await
            .unwrap();
        pb::process_client::ProcessClient::new(channel)
            .get_health(pb::GetHealthRequest::default())
            .await
    };
    let refused = health("http://evil.example:4000").await.unwrap_err();
    assert_eq!(refused.code(), tonic::Code::PermissionDenied, "{refused:?}");
    assert!(
        refused.message().contains("not an allowed name"),
        "{refused:?}"
    );
    assert!(health("http://hya.example.lan:4000").await.is_ok());
    assert!(health("http://localhost:1").await.is_ok());
    // The same rule on HTTP.
    let mut sender = http(addr).await;
    let mut rebound = request(addr, "GET", "/v1/health", None);
    rebound
        .headers_mut()
        .insert("host", "evil.example:4000".parse().unwrap());
    assert_eq!(sender.send_request(rebound).await.unwrap().status(), 403);
}
