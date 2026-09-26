//! The relay host connector (ADR-0025; docs/relay.md "Hosting a backend on
//! a relay"): a backend joins an in-process `hya proxy`, and a client
//! holding the link reaches the unchanged `/v1` router through the Noise
//! tunnel — REST, SSE, and the PTY WebSocket — while the loopback-only
//! `RelayControl` rpcs and process stop refuse relay-origin requests.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use http_body_util::{BodyExt as _, Full};
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_relay::client::{ClientConfig, ClientError, ReconnectPolicy, RelayClient};
use hya_relay::link::RelayLink;
use hya_relay::server::{RelayServer, RelayServerConfig};
use hya_relay::transport::ChunkTransport;
use hya_relay::tunnel::{NoiseStream, TunnelConfig};
use hya_server::relay_host::RelayState;
use hya_server::{AppState, RelayHost, RelayHostConfig, RelaySettings, ShutdownReason};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(15);

// ---- the relay (proxy) ----

struct Relay {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: JoinHandle<()>,
}

impl Relay {
    async fn start_on(addr: SocketAddr) -> Self {
        let (stop, stopped) = oneshot::channel::<()>();
        let (addr, serve) = RelayServer::bind(
            RelayServerConfig::new(addr).drain_timeout(Duration::from_millis(200)),
            async move {
                let _ = stopped.await;
            },
        )
        .await
        .unwrap();
        Self {
            addr,
            stop: Some(stop),
            done: tokio::spawn(serve),
        }
    }

    async fn start() -> Self {
        Self::start_on("127.0.0.1:0".parse().unwrap()).await
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let _ = tokio::time::timeout(WAIT, self.done).await;
    }
}

// ---- the backend ----

fn quick_policy() -> ReconnectPolicy {
    ReconnectPolicy {
        initial: Duration::from_millis(20),
        max: Duration::from_millis(200),
        conflict_initial: Duration::from_millis(50),
        conflict_max: Duration::from_millis(500),
        stable_after: Duration::from_secs(1),
    }
}

fn host_config(identity: Option<PathBuf>) -> RelayHostConfig {
    RelayHostConfig {
        identity_path: identity,
        reconnect: quick_policy(),
        shutdown_grace: Duration::from_millis(500),
        ..RelayHostConfig::default()
    }
}

struct Backend {
    state: AppState,
    relay: RelayHost,
    /// The loopback TCP listener serving the same router.
    local: String,
    dir: PathBuf,
}

async fn backend(config: RelayHostConfig) -> Backend {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    let relay = RelayHost::new(config);
    let state = AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
    .with_relay_host(relay.clone());
    // One server (HTTP and gRPC) on the loopback listener and the relay.
    let server = hya_server::build(state.clone());
    relay.set_service(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        server.serve(listener, std::future::pending()).await;
    });
    Backend {
        state,
        relay,
        local,
        dir: support::tempdir("relay-host"),
    }
}

async fn wait_state(relay: &RelayHost, state: RelayState) {
    tokio::time::timeout(WAIT, async {
        while relay.status().state != state {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("relay never reached {state:?}: {:?}", relay.status()));
}

async fn connected(backend: &Backend, relay: &Relay) -> RelayLink {
    let link = backend
        .relay
        .connect(RelaySettings::new(relay.url()))
        .await
        .unwrap();
    wait_state(&backend.relay, RelayState::Connected).await;
    link
}

// ---- a client holding the link ----

async fn tunnel(link: &RelayLink) -> Result<NoiseStream<ChunkTransport>, String> {
    let client =
        RelayClient::from_link(link, ClientConfig::default()).map_err(|e| e.to_string())?;
    let leg = client
        .open(link.room_id())
        .await
        .map_err(|e: ClientError| e.to_string())?;
    tokio::time::timeout(
        WAIT,
        NoiseStream::initiate_link(leg, link, TunnelConfig::default()),
    )
    .await
    .map_err(|_| "handshake timed out".to_owned())?
    .map_err(|e| e.to_string())
}

type Sender = hyper::client::conn::http1::SendRequest<Full<Bytes>>;

/// An HTTP/1.1 connection through the tunnel.
async fn http(link: &RelayLink) -> Sender {
    let io = TokioIo::new(tunnel(link).await.unwrap());
    let (sender, connection) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    sender
}

fn request(method: &str, uri: &str, body: Option<Value>) -> hyper::Request<Full<Bytes>> {
    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1");
    let bytes = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(body.to_string())
        }
        None => Bytes::new(),
    };
    builder.body(Full::new(bytes)).unwrap()
}

async fn call(sender: &mut Sender, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
    sender.ready().await.unwrap();
    let response = sender
        .send_request(request(method, uri, body))
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A gRPC channel (HTTP/2) through one tunnel stream.
async fn grpc(link: &RelayLink) -> tonic::transport::Channel {
    let io = tunnel(link).await.unwrap();
    let slot = Arc::new(std::sync::Mutex::new(Some(io)));
    tonic::transport::Endpoint::from_static("http://127.0.0.1:1")
        .connect_with_connector(tower::service_fn(move |_uri| {
            let io = slot.lock().unwrap().take();
            async move {
                io.map(TokioIo::new)
                    .ok_or_else(|| std::io::Error::other("the tunnel stream was used"))
            }
        }))
        .await
        .unwrap()
}

/// A plain loopback request to the backend's TCP listener.
async fn local_call(
    backend: &Backend,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let addr = backend.local.trim_start_matches("http://").to_owned();
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);
    call(&mut sender, method, path, body).await
}

/// Open an SSE stream through `sender`; returns its body.
async fn open_sse(sender: &mut Sender, uri: &str) -> hyper::body::Incoming {
    sender.ready().await.unwrap();
    let response = sender
        .send_request(request("GET", uri, None))
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{uri}");
    response.into_body()
}

/// Read SSE `data:` frames until one matches `pred` (or the body ends).
async fn sse_until(
    body: &mut hyper::body::Incoming,
    pred: impl Fn(&Value) -> bool,
) -> Option<Value> {
    let mut text = String::new();
    tokio::time::timeout(WAIT, async {
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame else { return None };
            let Some(data) = frame.data_ref() else {
                continue;
            };
            text.push_str(&String::from_utf8_lossy(data));
            while let Some(end) = text.find("\n\n") {
                let event: String = text.drain(..end + 2).collect();
                for line in event.lines() {
                    if let Some(data) = line.strip_prefix("data:")
                        && let Ok(value) = serde_json::from_str::<Value>(data.trim())
                        && pred(&value)
                    {
                        return Some(value);
                    }
                }
            }
        }
        None
    })
    .await
    .expect("sse frame in time")
}

/// Whether the SSE body ends (EOF or error) within `wait`.
async fn sse_ends(body: &mut hyper::body::Incoming, wait: Duration) -> bool {
    tokio::time::timeout(wait, async {
        while let Some(frame) = body.frame().await {
            if frame.is_err() {
                return;
            }
        }
    })
    .await
    .is_ok()
}

// ---- tests ----

#[tokio::test]
async fn a_link_holder_drives_rest_and_sse_through_the_tunnel() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    assert!(
        link.to_secret_string()
            .starts_with("hya+insecure://127.0.0.1:")
    );

    let mut api = http(&link).await;
    let (status, health) = call(&mut api, "GET", "/v1/health", None).await;
    assert_eq!(status, 200, "{health}");
    assert_eq!(health["ok"], json!(true));

    // A second connection streams the global events while the first one
    // creates a Project and a session in it.
    let mut events = http(&link).await;
    let mut stream = open_sse(&mut events, "/v1/events/stream").await;

    let root = backend.dir.to_string_lossy().into_owned();
    let (status, project) = call(
        &mut api,
        "POST",
        "/v1/projects",
        Some(json!({"name": "remote", "roots": [root]})),
    )
    .await;
    assert_eq!(status, 200, "{project}");
    let project_id = project["id"]
        .as_str()
        .or_else(|| project["project"]["id"].as_str())
        .unwrap()
        .to_owned();
    let (status, session) = call(
        &mut api,
        "POST",
        "/v1/sessions",
        Some(json!({"agent": "build", "model": "fake", "projectId": project_id})),
    )
    .await;
    assert_eq!(status, 200, "{session}");
    let session_id = session["session"]["id"].as_str().unwrap().to_owned();

    let frame = sse_until(&mut stream, |frame| frame.to_string().contains(&session_id)).await;
    assert!(frame.is_some(), "the SSE stream carried the new session");
    assert_eq!(backend.relay.status().active_streams, 2);
    relay.stop().await;
}

#[tokio::test]
async fn the_pty_websocket_upgrades_through_the_tunnel() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    let mut api = http(&link).await;
    let cwd = backend.dir.to_string_lossy().into_owned();
    let (status, pty) = call(
        &mut api,
        "POST",
        "/v1/pty",
        Some(json!({"shell": "/bin/sh", "cwd": cwd})),
    )
    .await;
    assert_eq!(status, 200, "{pty}");
    let id = pty["id"].as_str().unwrap().to_owned();
    let (status, token) = call(
        &mut api,
        "POST",
        &format!("/v1/pty/{id}/connect-token"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 200, "{token}");
    let path = token["url"].as_str().unwrap().to_owned();

    let io = tunnel(&link).await.unwrap();
    let (mut socket, response) =
        tokio_tungstenite::client_async(format!("ws://127.0.0.1{path}"), io)
            .await
            .unwrap();
    assert_eq!(response.status(), 101);
    let input = json!({"input": base64_of(b"echo relay-pty-ok\n")}).to_string();
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(input))
        .await
        .unwrap();
    let seen = tokio::time::timeout(WAIT, async {
        let mut output = Vec::new();
        while let Some(Ok(message)) = socket.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if let Some(bytes) = frame["output"].as_str() {
                    output.extend(base64_decode(bytes));
                    if String::from_utf8_lossy(&output).contains("relay-pty-ok") {
                        return true;
                    }
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(seen, "the shell's output came back through the tunnel");
    relay.stop().await;
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap_or_default()
}

#[tokio::test]
async fn relay_control_and_process_stop_are_loopback_only() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    let mut api = http(&link).await;

    for (method, path, body) in [
        ("GET", "/v1/relay/status", None),
        ("GET", "/v1/relay/link", None),
        ("POST", "/v1/relay/rotate", Some(json!({}))),
        ("POST", "/v1/relay/disconnect", Some(json!({}))),
        (
            "POST",
            "/v1/relay/connect",
            Some(json!({"proxyUrl": relay.url()})),
        ),
        ("POST", "/v1/process/dispose", Some(json!({}))),
        ("POST", "/v1/process/upgrade", Some(json!({}))),
    ] {
        let (status, error) = call(&mut api, method, path, body).await;
        assert_eq!(status, 403, "{method} {path}: {error}");
        assert_eq!(error["error"]["code"], json!("permission_denied"), "{path}");
    }
    // Still connected, same link.
    assert_eq!(backend.relay.status().state, RelayState::Connected);

    let (status, body) = local_call(&backend, "GET", "/v1/relay/status", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], json!("RELAY_STATE_CONNECTED"));
    assert_eq!(body["roomId"], json!(link.room_id().as_str()));
    assert_eq!(body["transport"], json!("auto"));
    assert!(!body.to_string().contains(&link.to_secret_string()));
    let (status, body) = local_call(&backend, "GET", "/v1/relay/link", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["link"], json!(link.to_secret_string()));

    // A browser request is refused even on loopback.
    let addr = backend.local.trim_start_matches("http://").to_owned();
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);
    let mut browser = request("GET", "/v1/relay/link", None);
    browser
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let response = sender.send_request(browser).await.unwrap();
    assert_eq!(response.status(), 403);
    relay.stop().await;
}

/// gRPC reaches the same server through the tunnel, and every call keeps
/// the relay origin: `RelayControl` and process stop/upgrade are refused
/// with `PERMISSION_DENIED` (also in the in-process dispatch), browser
/// markers are refused, and the same rpcs over loopback work.
#[tokio::test]
async fn grpc_over_the_relay_keeps_the_relay_origin() {
    use hya_api::v1 as pb;
    use pb::process_client::ProcessClient;
    use pb::relay_control_client::RelayControlClient;

    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    let channel = grpc(&link).await;

    let health = ProcessClient::new(channel.clone())
        .get_health(pb::GetHealthRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert!(health.ok);
    let created = pb::session_client::SessionClient::new(channel.clone())
        .create_session(pb::CreateSessionRequest {
            agent: "build".to_owned(),
            model: "fake".to_owned(),
            workdir: Some(backend.dir.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert!(created.session.is_some());

    let denied = |result: Result<(), tonic::Status>, what: &str| {
        let status = result.expect_err(what);
        assert_eq!(
            status.code(),
            tonic::Code::PermissionDenied,
            "{what}: {status:?}"
        );
        assert!(status.message().contains("relay"), "{what}: {status:?}");
    };
    let mut control = RelayControlClient::new(channel.clone());
    denied(
        control
            .get_relay_status(pb::GetRelayStatusRequest::default())
            .await
            .map(drop),
        "status",
    );
    denied(
        control
            .get_relay_link(pb::GetRelayLinkRequest::default())
            .await
            .map(drop),
        "link",
    );
    denied(
        control
            .rotate_relay_key(pb::RotateRelayKeyRequest::default())
            .await
            .map(drop),
        "rotate",
    );
    denied(
        control
            .disconnect_relay(pb::DisconnectRelayRequest::default())
            .await
            .map(drop),
        "disconnect",
    );
    denied(
        control
            .connect_relay(pb::ConnectRelayRequest {
                proxy_url: relay.url(),
                ..Default::default()
            })
            .await
            .map(drop),
        "connect",
    );
    // Process stop/upgrade reach the router's relay check through the
    // in-process dispatch.
    let mut process = ProcessClient::new(channel.clone());
    denied(
        process
            .dispose_process(pb::DisposeProcessRequest::default())
            .await
            .map(drop),
        "dispose",
    );
    denied(
        process
            .upgrade_process(pb::UpgradeProcessRequest::default())
            .await
            .map(drop),
        "upgrade",
    );
    // A browser marker over the relay is refused for gRPC too.
    let mut browser = tonic::Request::new(pb::GetHealthRequest::default());
    browser
        .metadata_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let refused = process.get_health(browser).await.unwrap_err();
    assert_eq!(refused.code(), tonic::Code::PermissionDenied, "{refused:?}");
    assert!(refused.message().contains("browser"), "{refused:?}");
    assert_eq!(backend.relay.status().state, RelayState::Connected);

    // Over loopback the same rpcs work.
    let local = tonic::transport::Channel::from_shared(backend.local.clone())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let status = RelayControlClient::new(local.clone())
        .get_relay_status(pb::GetRelayStatusRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(status.room_id, link.room_id().as_str());
    let got = RelayControlClient::new(local)
        .get_relay_link(pb::GetRelayLinkRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.link, link.to_secret_string());
    relay.stop().await;
}

#[tokio::test]
async fn browser_requests_and_foreign_hosts_are_refused_over_the_relay() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    let mut api = http(&link).await;
    // A browser fetch (CORS or not) carries Origin or Sec-Fetch-*.
    for (name, value) in [
        ("origin", "https://evil.example"),
        ("origin", "null"),
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-mode", "cors"),
    ] {
        let mut browser = request("GET", "/v1/health", None);
        browser.headers_mut().insert(name, value.parse().unwrap());
        api.ready().await.unwrap();
        let response = api.send_request(browser).await.unwrap();
        assert_eq!(response.status(), 403, "{name}: {value}");
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["error"]["code"], json!("permission_denied"));
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("browser requests are not accepted over the relay"),
            "{body}"
        );
    }
    // The CORS preflight is refused too (it never gets the mirrored headers).
    let mut preflight = request("OPTIONS", "/v1/sessions", None);
    preflight
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    preflight
        .headers_mut()
        .insert("access-control-request-method", "POST".parse().unwrap());
    api.ready().await.unwrap();
    let response = api.send_request(preflight).await.unwrap();
    assert_eq!(response.status(), 403);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    // A rebound Host through the bridge.
    let mut rebound = request("GET", "/v1/health", None);
    rebound
        .headers_mut()
        .insert("host", "evil.example:4000".parse().unwrap());
    api.ready().await.unwrap();
    assert_eq!(api.send_request(rebound).await.unwrap().status(), 403);
    // A plain client through the relay still works.
    let (status, body) = call(&mut api, "GET", "/v1/health", None).await;
    assert_eq!(status, 200, "{body}");

    // A browser WebSocket handshake always carries Origin.
    let (status, pty) = call(
        &mut api,
        "POST",
        "/v1/pty",
        Some(json!({"shell": "/bin/sh", "cwd": backend.dir.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, 200, "{pty}");
    let id = pty["id"].as_str().unwrap().to_owned();
    let (status, token) = call(
        &mut api,
        "POST",
        &format!("/v1/pty/{id}/connect-token"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 200, "{token}");
    let path = token["url"].as_str().unwrap().to_owned();
    let io = tunnel(&link).await.unwrap();
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut upgrade = format!("ws://127.0.0.1{path}")
        .into_client_request()
        .unwrap();
    upgrade
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let refused = tokio_tungstenite::client_async(upgrade, io).await;
    match refused {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 403);
        }
        other => panic!("the browser WebSocket upgrade was not refused: {other:?}"),
    }
    relay.stop().await;
}

#[tokio::test]
async fn rotating_the_key_closes_streams_and_invalidates_the_old_link() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let old = connected(&backend, &relay).await;
    let mut events = http(&old).await;
    let mut stream = open_sse(&mut events, "/v1/events/stream").await;

    let (status, body) = local_call(&backend, "POST", "/v1/relay/rotate", Some(json!({}))).await;
    assert_eq!(status, 200, "{body}");
    let new: RelayLink = body["link"].as_str().unwrap().parse().unwrap();
    assert_ne!(new.psk(), old.psk());
    assert_eq!(new.room_id(), old.room_id());
    assert_eq!(new.server_key(), old.server_key());

    assert!(
        sse_ends(&mut stream, Duration::from_secs(5)).await,
        "the open stream of the old link was closed"
    );
    // The proxy already refuses the old link's open token: the backend
    // never even sees the stream.
    let error = tunnel(&old).await.expect_err("the old link is rejected");
    assert!(error.contains("offline"), "{error}");
    let mut api = http(&new).await;
    let (status, _) = call(&mut api, "GET", "/v1/health", None).await;
    assert_eq!(status, 200);
    relay.stop().await;
}

#[tokio::test]
async fn connect_disconnect_and_status_on_a_running_server() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let (status, body) = local_call(&backend, "GET", "/v1/relay/status", None).await;
    assert_eq!(status, 200);
    assert_eq!(body["state"], json!("RELAY_STATE_DISCONNECTED"));
    let (status, body) = local_call(&backend, "GET", "/v1/relay/link", None).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], json!("failed_precondition"));

    let (status, body) = local_call(
        &backend,
        "POST",
        "/v1/relay/connect",
        Some(json!({"proxyUrl": "ftp://nope"})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = local_call(
        &backend,
        "POST",
        "/v1/relay/connect",
        Some(json!({"proxyUrl": relay.url(), "transport": "ws"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let link: RelayLink = body["link"].as_str().unwrap().parse().unwrap();
    assert!(body["link"].as_str().unwrap().contains("t=ws"));
    wait_state(&backend.relay, RelayState::Connected).await;
    let (_, body) = local_call(&backend, "GET", "/v1/relay/status", None).await;
    assert_eq!(body["binding"], json!("ws"));
    assert!(body["connectedSince"].is_string(), "{body}");
    assert_eq!(body["ephemeral"], json!(true), "an in-memory database");
    let mut api = http(&link).await;
    assert_eq!(call(&mut api, "GET", "/v1/health", None).await.0, 200);

    let (status, body) =
        local_call(&backend, "POST", "/v1/relay/disconnect", Some(json!({}))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], json!("RELAY_STATE_DISCONNECTED"));
    let error = tunnel(&link).await.expect_err("the room is released");
    assert!(error.contains("offline"), "{error}");
    relay.stop().await;
}

#[tokio::test]
async fn the_identity_file_keeps_the_link_across_restarts() {
    let relay = Relay::start().await;
    let dir = support::tempdir("relay-identity");
    let path = dir.join("sessions.db.relay-identity.json");
    let first = backend(host_config(Some(path.clone()))).await;
    let link = connected(&first, &relay).await;
    assert!(!first.relay.status().ephemeral);
    first.relay.shutdown().await;
    drop(first);

    let second = backend(host_config(Some(path.clone()))).await;
    let again = connected(&second, &relay).await;
    assert_eq!(again.to_secret_string(), link.to_secret_string());
    let mut api = http(&link).await;
    assert_eq!(call(&mut api, "GET", "/v1/health", None).await.0, 200);

    // --relay-ephemeral: a throwaway identity, not the file's.
    let mut settings = RelaySettings::new(relay.url());
    settings.ephemeral = true;
    let throwaway = second.relay.connect(settings).await.unwrap();
    assert_ne!(throwaway.room_id(), link.room_id());
    assert!(second.relay.status().ephemeral);
    relay.stop().await;
}

#[tokio::test]
async fn the_connector_re_registers_after_the_proxy_restarts() {
    let relay = Relay::start().await;
    let addr = relay.addr;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    relay.stop().await;
    wait_state(&backend.relay, RelayState::Backoff).await;
    assert!(backend.relay.status().last_error.is_some());

    let relay = Relay::start_on(addr).await;
    wait_state(&backend.relay, RelayState::Connected).await;
    let mut api = http(&link).await;
    assert_eq!(call(&mut api, "GET", "/v1/health", None).await.0, 200);
    relay.stop().await;
}

#[tokio::test]
async fn server_stopping_reaches_a_relay_side_sse_client_and_the_room_is_released() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    let mut events = http(&link).await;
    let mut stream = open_sse(&mut events, "/v1/events/stream").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    backend.state.streams().close(ShutdownReason::Stop);
    let last = sse_until(&mut stream, |frame| {
        frame["event"]["serverStopping"].is_object()
    })
    .await;
    assert_eq!(
        last.expect("serverStopping frame")["event"]["serverStopping"],
        json!({"reason": "stop"})
    );
    backend.relay.shutdown().await;
    assert_eq!(backend.relay.status().state, RelayState::Disconnected);
    assert!(sse_ends(&mut stream, Duration::from_secs(5)).await);
    let error = tunnel(&link).await.expect_err("the room is released");
    assert!(error.contains("offline"), "{error}");
    // Connecting after shutdown is refused.
    assert!(
        backend
            .relay
            .connect(RelaySettings::new(relay.url()))
            .await
            .is_err()
    );
    relay.stop().await;
}

#[tokio::test]
async fn stalled_handshakes_have_their_own_budget_and_never_block_serving_streams() {
    let relay = Relay::start().await;
    let backend = backend(RelayHostConfig {
        max_streams: 2,
        max_handshakes: 2,
        handshake_timeout: Duration::from_secs(1),
        ..host_config(None)
    })
    .await;
    let link = connected(&backend, &relay).await;
    let mut first = http(&link).await;
    assert_eq!(call(&mut first, "GET", "/v1/health", None).await.0, 200);

    // Two link holders open streams and never send the Noise hello: they
    // fill the handshake budget, not the serving slots.
    let client = RelayClient::from_link(&link, ClientConfig::default()).unwrap();
    let stalled_a = tokio::time::timeout(WAIT, client.open(link.room_id()))
        .await
        .unwrap()
        .unwrap();
    let stalled_b = tokio::time::timeout(WAIT, client.open(link.room_id()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(call(&mut first, "GET", "/v1/health", None).await.0, 200);

    // Once they time out, a second serving stream fits beside the first.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut second = http(&link).await;
    assert_eq!(call(&mut second, "GET", "/v1/health", None).await.0, 200);
    assert_eq!(call(&mut first, "GET", "/v1/health", None).await.0, 200);
    assert_eq!(backend.relay.status().active_streams, 2);
    drop((stalled_a, stalled_b));
    relay.stop().await;
}

#[tokio::test]
async fn a_stalled_handshake_does_not_take_a_serving_slot() {
    let relay = Relay::start().await;
    let backend = backend(RelayHostConfig {
        max_streams: 2,
        max_handshakes: 2,
        handshake_timeout: Duration::from_secs(10),
        ..host_config(None)
    })
    .await;
    let link = connected(&backend, &relay).await;
    let mut first = http(&link).await;
    assert_eq!(call(&mut first, "GET", "/v1/health", None).await.0, 200);
    let client = RelayClient::from_link(&link, ClientConfig::default()).unwrap();
    let stalled = tokio::time::timeout(WAIT, client.open(link.room_id()))
        .await
        .unwrap()
        .unwrap();
    // While it stalls, the second serving slot is still free.
    let mut second = tokio::time::timeout(Duration::from_secs(5), http(&link))
        .await
        .expect("a serving slot was free");
    assert_eq!(call(&mut second, "GET", "/v1/health", None).await.0, 200);
    drop(stalled);
    relay.stop().await;
}
