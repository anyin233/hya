//! Host/Origin admission guard over HTTP & gRPC (`HostPolicy`, DNS rebinding,
//! browser markers, loopback-only `RelayControl`) and the relay host connector
//! over an in-process `hya proxy` (REST, SSE, PTY WebSocket, gRPC, key
//! rotation, reconnect, shutdown, and handshake budgets).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use http_body_util::{BodyExt as _, Full};
use hya_api::v2 as pb;
use hya_relay::client::{ClientConfig, ClientError, ReconnectPolicy, RelayClient};
use hya_relay::link::RelayLink;
use hya_relay::server::{RelayServer, RelayServerConfig};
use hya_relay::transport::ChunkTransport;
use hya_relay::tunnel::{NoiseStream, TunnelConfig};
use hya_server::relay_host::RelayState;
use hya_server::{
    AppState, HostPolicy, RelayHost, RelayHostConfig, RelaySettings, ShutdownReason, build, router,
};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower::ServiceExt as _;

use crate::harness::{b64_decode, b64_encode, base_state, id_str, serve_server};
use crate::support;

const WAIT: Duration = Duration::from_secs(10);

// ---- Host Guard (HTTP + gRPC) ----

async fn raw_http(addr: SocketAddr, head: &str) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

fn raw_get(path: &str, host: Option<&str>) -> String {
    let host = host.map_or_else(String::new, |h| format!("Host: {h}\r\n"));
    format!("GET {path} HTTP/1.1\r\n{host}Connection: close\r\n\r\n")
}

#[tokio::test]
async fn host_guard_enforces_allowlist_over_http_and_grpc() {
    let loopback = build(
        base_state()
            .await
            .with_allowed_hosts(HostPolicy::loopback()),
    );
    let (addr, _t1) = serve_server(&loopback, std::future::pending()).await;
    for host in [
        format!("127.0.0.1:{}", addr.port()),
        format!("localhost:{}", addr.port()),
        "localhost".into(),
        "[::1]:9".into(),
    ] {
        assert_eq!(
            raw_http(addr, &raw_get("/v2/health", Some(&host))).await.0,
            200,
            "{host}"
        );
    }
    for path in ["/v2/relay/link", "/v2/health", "/v2/sessions", "/nope"] {
        let (s, body) = raw_http(
            addr,
            &raw_get(path, Some(&format!("evil.example:{}", addr.port()))),
        )
        .await;
        assert_eq!(
            (s, &body["error"]["code"]),
            (403, &json!("permission_denied"))
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("--allow-host")
        );
    }
    assert_eq!(raw_http(addr, "OPTIONS /v2/relay/link HTTP/1.1\r\nHost: evil.example\r\nOrigin: http://evil.example\r\nAccess-Control-Request-Method: GET\r\nConnection: close\r\n\r\n").await.0, 403);
    let (s, body) = raw_http(addr, "GET /v2/health HTTP/1.0\r\n\r\n").await;
    assert_eq!(
        (s, &body["error"]["code"]),
        (403, &json!("permission_denied"))
    );

    // In-process request without ConnectInfo fails closed on relay control, passes health.
    let app = router(
        base_state()
            .await
            .with_allowed_hosts(HostPolicy::loopback()),
    );
    assert_eq!(
        app.clone()
            .oneshot(
                axum::http::Request::get("/v2/relay/link")
                    .body(axum::body::Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        app.oneshot(
            axum::http::Request::get("/v2/health")
                .body(axum::body::Body::empty())
                .unwrap()
        )
        .await
        .unwrap()
        .status(),
        200
    );

    // Configured HostPolicy extends loopback names on HTTP and gRPC `:authority`.
    let configured = build(
        base_state()
            .await
            .with_allowed_hosts(HostPolicy::with_hosts(["hya.example.lan"]).unwrap()),
    );
    let (cfg_addr, _t2) = serve_server(&configured, std::future::pending()).await;
    assert_eq!(
        raw_http(
            cfg_addr,
            &raw_get("/v2/health", Some("HYA.example.lan:8080"))
        )
        .await
        .0,
        200
    );
    let (s, body) = raw_http(cfg_addr, &raw_get("/v2/health", Some("other.example.lan"))).await;
    assert_eq!(s, 403);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hya.example.lan")
    );

    let grpc_health = |origin: &'static str| async move {
        let ch = tonic::transport::Endpoint::from_shared(format!("http://{cfg_addr}"))
            .unwrap()
            .origin(origin.parse().unwrap())
            .connect()
            .await
            .unwrap();
        pb::process_client::ProcessClient::new(ch)
            .get_health(pb::GetHealthRequest::default())
            .await
    };
    let err = grpc_health("http://evil.example:4000").await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(err.message().contains("not an allowed name"));
    assert!(grpc_health("http://hya.example.lan:4000").await.is_ok());
    assert!(grpc_health("http://localhost:1").await.is_ok());
}

// ---- Relay Proxy & Backend Fixtures ----

struct Relay {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: JoinHandle<()>,
}

impl Relay {
    async fn start_on(addr: SocketAddr) -> Self {
        let (stop, stopped) = oneshot::channel::<()>();
        let (addr, serve) = RelayServer::bind(
            RelayServerConfig::new(addr).drain_timeout(Duration::from_millis(100)),
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

fn host_config(identity: Option<PathBuf>) -> RelayHostConfig {
    RelayHostConfig {
        identity_path: identity,
        reconnect: ReconnectPolicy {
            initial: Duration::from_millis(20),
            max: Duration::from_millis(150),
            conflict_initial: Duration::from_millis(30),
            conflict_max: Duration::from_millis(200),
            stable_after: Duration::from_millis(500),
        },
        shutdown_grace: Duration::from_millis(150),
        ..RelayHostConfig::default()
    }
}

struct Backend {
    state: AppState,
    relay: RelayHost,
    local: String,
    dir: PathBuf,
}
impl Drop for Backend {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn backend(config: RelayHostConfig) -> Backend {
    let relay = RelayHost::new(config);
    let state = base_state().await.with_relay_host(relay.clone());
    let server = build(state.clone());
    relay.set_service(server.clone());
    let (addr, _task) = serve_server(&server, std::future::pending()).await;
    Backend {
        state,
        relay,
        local: format!("http://{addr}"),
        dir: support::tempdir("relay-host"),
    }
}

async fn wait_state(relay: &RelayHost, state: RelayState) {
    tokio::time::timeout(WAIT, async {
        while relay.status().state != state {
            tokio::time::sleep(Duration::from_millis(5)).await;
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

async fn tunnel(link: &RelayLink) -> Result<NoiseStream<ChunkTransport>, String> {
    let leg = RelayClient::from_link(link, ClientConfig::default())
        .map_err(|e| e.to_string())?
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

async fn http_tunnel(link: &RelayLink) -> Sender {
    let (sender, conn) =
        hyper::client::conn::http1::handshake(TokioIo::new(tunnel(link).await.unwrap()))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    sender
}

fn req(method: &str, uri: &str, body: Option<Value>) -> hyper::Request<Full<Bytes>> {
    let mut b = hyper::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1");
    let bytes = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Bytes::from(v.to_string())
        }
        None => Bytes::new(),
    };
    b.body(Full::new(bytes)).unwrap()
}

async fn call_h1(
    sender: &mut Sender,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    sender.ready().await.unwrap();
    let resp = sender.send_request(req(method, uri, body)).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn ok_h1(sender: &mut Sender, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (s, v) = call_h1(sender, method, uri, body).await;
    assert_eq!(s, 200, "{method} {uri}: {v}");
    v
}

async fn local_call(
    backend: &Backend,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let stream = tokio::net::TcpStream::connect(backend.local.trim_start_matches("http://"))
        .await
        .unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    call_h1(&mut sender, method, path, body).await
}

async fn grpc_tunnel(link: &RelayLink) -> tonic::transport::Channel {
    let slot = Arc::new(std::sync::Mutex::new(Some(tunnel(link).await.unwrap())));
    tonic::transport::Endpoint::from_static("http://127.0.0.1:1")
        .connect_with_connector(tower::service_fn(move |_| {
            let io = slot.lock().unwrap().take();
            async move {
                io.map(TokioIo::new)
                    .ok_or_else(|| std::io::Error::other("used"))
            }
        }))
        .await
        .unwrap()
}

async fn open_sse(sender: &mut Sender, uri: &str) -> hyper::body::Incoming {
    sender.ready().await.unwrap();
    let resp = sender.send_request(req("GET", uri, None)).await.unwrap();
    assert_eq!(resp.status(), 200, "{uri}");
    resp.into_body()
}

async fn sse_until(
    body: &mut hyper::body::Incoming,
    pred: impl Fn(&Value) -> bool,
) -> Option<Value> {
    let mut text = String::new();
    tokio::time::timeout(WAIT, async {
        while let Some(Ok(frame)) = body.frame().await {
            let Some(data) = frame.data_ref() else {
                continue;
            };
            text.push_str(&String::from_utf8_lossy(data));
            while let Some(end) = text.find("\n\n") {
                for line in text.drain(..end + 2).collect::<String>().lines() {
                    if let Some(d) = line.strip_prefix("data:")
                        && let Ok(v) = serde_json::from_str::<Value>(d.trim())
                        && pred(&v)
                    {
                        return Some(v);
                    }
                }
            }
        }
        None
    })
    .await
    .expect("sse frame in time")
}

async fn sse_ends(body: &mut hyper::body::Incoming, wait: Duration) -> bool {
    tokio::time::timeout(wait, async {
        while let Some(Ok(_)) = body.frame().await {}
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn relay_tunnel_serves_rest_sse_and_pty_websocket() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    assert!(
        link.to_secret_string()
            .starts_with("hya+insecure://127.0.0.1:")
    );

    let mut api = http_tunnel(&link).await;
    assert_eq!(
        ok_h1(&mut api, "GET", "/v2/health", None).await["ok"],
        json!(true)
    );
    let mut events = http_tunnel(&link).await;
    let mut stream = open_sse(&mut events, "/v2/events/stream").await;

    let root = backend.dir.to_string_lossy().into_owned();
    let pid = id_str(
        &ok_h1(
            &mut api,
            "POST",
            "/v2/projects",
            Some(json!({"name": "remote", "roots": [root]})),
        )
        .await["id"],
    );
    let sid = id_str(
        &ok_h1(
            &mut api,
            "POST",
            "/v2/sessions",
            Some(json!({"agent": "build", "model": "fake", "projectId": pid})),
        )
        .await["session"]["id"],
    );
    assert!(
        sse_until(&mut stream, |f| f.to_string().contains(&sid))
            .await
            .is_some()
    );
    assert_eq!(backend.relay.status().active_streams, 2);

    // PTY WebSocket upgrade over tunnel.
    let id = id_str(
        &ok_h1(
            &mut api,
            "POST",
            "/v2/pty",
            Some(json!({"shell": "/bin/sh", "cwd": root})),
        )
        .await["id"],
    );
    let path = id_str(
        &ok_h1(
            &mut api,
            "POST",
            &format!("/v2/pty/{id}/connect-token"),
            Some(json!({})),
        )
        .await["url"],
    );
    let (mut socket, resp) = tokio_tungstenite::client_async(
        format!("ws://127.0.0.1{path}"),
        tunnel(&link).await.unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), 101);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"input": b64_encode(b"echo relay-pty-ok\n")}).to_string(),
        ))
        .await
        .unwrap();
    let seen = tokio::time::timeout(WAIT, async {
        let mut out = Vec::new();
        while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) = socket.next().await {
            if let Some(b) = serde_json::from_str::<Value>(&t).unwrap()["output"].as_str() {
                out.extend(b64_decode(b));
                if String::from_utf8_lossy(&out).contains("relay-pty-ok") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(seen);
    relay.stop().await;
}

#[tokio::test]
async fn relay_refuses_control_rpcs_browser_headers_foreign_hosts_and_browser_ws() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    let mut api = http_tunnel(&link).await;

    for (method, path, body) in [
        ("GET", "/v2/relay/status", None),
        ("GET", "/v2/relay/link", None),
        ("POST", "/v2/relay/rotate", Some(json!({}))),
        ("POST", "/v2/relay/disconnect", Some(json!({}))),
        (
            "POST",
            "/v2/relay/connect",
            Some(json!({"proxyUrl": relay.url()})),
        ),
    ] {
        let (s, err) = call_h1(&mut api, method, path, body).await;
        assert_eq!(
            (s, &err["error"]["code"]),
            (403, &json!("permission_denied"))
        );
    }
    assert_eq!(backend.relay.status().state, RelayState::Connected);

    let (s, st) = local_call(&backend, "GET", "/v2/relay/status", None).await;
    assert_eq!((s, &st["state"]), (200, &json!("RELAY_STATE_CONNECTED")));
    assert!(!st.to_string().contains(&link.to_secret_string()));
    let (s, lnk) = local_call(&backend, "GET", "/v2/relay/link", None).await;
    assert_eq!((s, &lnk["link"]), (200, &json!(link.to_secret_string())));

    // Browser headers, CORS preflight, and foreign Host refused over relay.
    for (name, val) in [
        ("origin", "https://evil.example"),
        ("origin", "null"),
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-mode", "cors"),
        ("host", "evil.example:4000"),
    ] {
        let mut r = req("GET", "/v2/health", None);
        r.headers_mut().insert(name, val.parse().unwrap());
        api.ready().await.unwrap();
        assert_eq!(
            api.send_request(r).await.unwrap().status(),
            403,
            "{name}: {val}"
        );
    }
    let mut preflight = req("OPTIONS", "/v2/sessions", None);
    preflight
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    preflight
        .headers_mut()
        .insert("access-control-request-method", "POST".parse().unwrap());
    api.ready().await.unwrap();
    let resp = api.send_request(preflight).await.unwrap();
    assert_eq!(resp.status(), 403);
    assert!(resp.headers().get("access-control-allow-origin").is_none());

    // Browser WebSocket upgrade carrying Origin is refused.
    let pty = ok_h1(
        &mut api,
        "POST",
        "/v2/pty",
        Some(json!({"shell": "/bin/sh", "cwd": backend.dir.to_string_lossy()})),
    )
    .await;
    let tok = ok_h1(
        &mut api,
        "POST",
        &format!("/v2/pty/{}/connect-token", pty["id"].as_str().unwrap()),
        Some(json!({})),
    )
    .await;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut upgrade = format!("ws://127.0.0.1{}", tok["url"].as_str().unwrap())
        .into_client_request()
        .unwrap();
    upgrade
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    match tokio_tungstenite::client_async(upgrade, tunnel(&link).await.unwrap()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403),
        other => panic!("expected 403 WS refusal, got {other:?}"),
    }
    relay.stop().await;
}

#[tokio::test]
async fn relay_tunnel_grpc_keeps_relay_origin_and_refuses_control_and_browser() {
    use pb::{process_client::ProcessClient, relay_control_client::RelayControlClient};

    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;
    let ch = grpc_tunnel(&link).await;

    assert!(
        ProcessClient::new(ch.clone())
            .get_health(pb::GetHealthRequest::default())
            .await
            .unwrap()
            .into_inner()
            .ok
    );
    assert!(
        pb::session_client::SessionClient::new(ch.clone())
            .create_session(pb::CreateSessionRequest {
                agent: "build".into(),
                model: "fake".into(),
                workdir: Some(backend.dir.to_string_lossy().into_owned()),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner()
            .session
            .is_some()
    );

    let mut ctrl = RelayControlClient::new(ch.clone());
    for err in [
        ctrl.get_relay_status(pb::GetRelayStatusRequest::default())
            .await
            .unwrap_err(),
        ctrl.get_relay_link(pb::GetRelayLinkRequest::default())
            .await
            .unwrap_err(),
        ctrl.rotate_relay_key(pb::RotateRelayKeyRequest::default())
            .await
            .unwrap_err(),
        ctrl.disconnect_relay(pb::DisconnectRelayRequest::default())
            .await
            .unwrap_err(),
        ctrl.connect_relay(pb::ConnectRelayRequest {
            proxy_url: relay.url(),
            ..Default::default()
        })
        .await
        .unwrap_err(),
    ] {
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        assert!(err.message().contains("relay"));
    }

    let mut browser = tonic::Request::new(pb::GetHealthRequest::default());
    browser
        .metadata_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let err = ProcessClient::new(ch)
        .get_health(browser)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(err.message().contains("browser"));

    let local = tonic::transport::Channel::from_shared(backend.local.clone())
        .unwrap()
        .connect()
        .await
        .unwrap();
    assert_eq!(
        RelayControlClient::new(local)
            .get_relay_link(pb::GetRelayLinkRequest::default())
            .await
            .unwrap()
            .into_inner()
            .link,
        link.to_secret_string()
    );
    relay.stop().await;
}

#[tokio::test]
async fn relay_rotate_key_closes_streams_and_invalidates_old_link() {
    let relay = Relay::start().await;
    let backend = backend(host_config(None)).await;
    let old = connected(&backend, &relay).await;
    let mut events = http_tunnel(&old).await;
    let mut stream = open_sse(&mut events, "/v2/events/stream").await;

    let (status, body) = local_call(&backend, "POST", "/v2/relay/rotate", Some(json!({}))).await;
    assert_eq!(status, 200, "{body}");
    let new: RelayLink = body["link"].as_str().unwrap().parse().unwrap();
    assert_ne!(new.psk(), old.psk());
    assert_eq!(
        (new.room_id(), new.server_key()),
        (old.room_id(), old.server_key())
    );
    assert!(sse_ends(&mut stream, Duration::from_secs(5)).await);
    assert!(tunnel(&old).await.unwrap_err().contains("offline"));
    let _ = ok_h1(&mut http_tunnel(&new).await, "GET", "/v2/health", None).await;
    relay.stop().await;
}

#[tokio::test]
async fn relay_connect_disconnect_status_and_identity_persistence() {
    let relay = Relay::start().await;
    let b = backend(host_config(None)).await;
    assert_eq!(
        local_call(&b, "GET", "/v2/relay/status", None).await.1["state"],
        json!("RELAY_STATE_DISCONNECTED")
    );
    let (s, err) = local_call(&b, "GET", "/v2/relay/link", None).await;
    assert_eq!(
        (s, &err["error"]["code"]),
        (409, &json!("failed_precondition"))
    );
    assert_eq!(
        local_call(
            &b,
            "POST",
            "/v2/relay/connect",
            Some(json!({"proxyUrl": "ftp://nope"}))
        )
        .await
        .0,
        400
    );

    let (s, body) = local_call(
        &b,
        "POST",
        "/v2/relay/connect",
        Some(json!({"proxyUrl": relay.url(), "transport": "ws"})),
    )
    .await;
    assert_eq!(s, 200, "{body}");
    let link: RelayLink = body["link"].as_str().unwrap().parse().unwrap();
    assert!(body["link"].as_str().unwrap().contains("t=ws"));
    wait_state(&b.relay, RelayState::Connected).await;
    let (_, st) = local_call(&b, "GET", "/v2/relay/status", None).await;
    assert_eq!(
        (&st["binding"], &st["ephemeral"]),
        (&json!("ws"), &json!(true))
    );
    let _ = ok_h1(&mut http_tunnel(&link).await, "GET", "/v2/health", None).await;
    assert_eq!(
        local_call(&b, "POST", "/v2/relay/disconnect", Some(json!({})))
            .await
            .1["state"],
        json!("RELAY_STATE_DISCONNECTED")
    );
    assert!(tunnel(&link).await.unwrap_err().contains("offline"));

    // Identity file keeps the link across restarts; ephemeral flag ignores file.
    let dir = support::tempdir("relay-identity");
    let path = dir.join("sessions.db.relay-identity.json");
    let first = backend(host_config(Some(path.clone()))).await;
    let saved = connected(&first, &relay).await;
    assert!(!first.relay.status().ephemeral);
    first.relay.shutdown().await;
    drop(first);

    let second = backend(host_config(Some(path))).await;
    assert_eq!(
        connected(&second, &relay).await.to_secret_string(),
        saved.to_secret_string()
    );
    let mut settings = RelaySettings::new(relay.url());
    settings.ephemeral = true;
    assert_ne!(
        second.relay.connect(settings).await.unwrap().room_id(),
        saved.room_id()
    );
    assert!(second.relay.status().ephemeral);
    let _ = std::fs::remove_dir_all(&dir);
    relay.stop().await;
}

#[tokio::test]
async fn relay_reregisters_after_proxy_restart_and_shutdown_sends_server_stopping() {
    let relay = Relay::start().await;
    let addr = relay.addr;
    let backend = backend(host_config(None)).await;
    let link = connected(&backend, &relay).await;

    relay.stop().await;
    wait_state(&backend.relay, RelayState::Backoff).await;
    assert!(backend.relay.status().last_error.is_some());
    let relay = Relay::start_on(addr).await;
    wait_state(&backend.relay, RelayState::Connected).await;
    let _ = ok_h1(&mut http_tunnel(&link).await, "GET", "/v2/health", None).await;

    let mut events = http_tunnel(&link).await;
    let mut stream = open_sse(&mut events, "/v2/events/stream").await;
    backend.state.streams().close(ShutdownReason::Stop);
    let last = sse_until(&mut stream, |f| f["event"]["serverStopping"].is_object()).await;
    assert_eq!(
        last.unwrap()["event"]["serverStopping"],
        json!({"reason": "stop"})
    );
    backend.relay.shutdown().await;
    assert_eq!(backend.relay.status().state, RelayState::Disconnected);
    assert!(sse_ends(&mut stream, Duration::from_secs(5)).await);
    assert!(tunnel(&link).await.unwrap_err().contains("offline"));
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
async fn relay_stalled_handshakes_use_separate_budget_and_timeout_frees_slots() {
    let relay = Relay::start().await;
    let backend = backend(RelayHostConfig {
        max_streams: 2,
        max_handshakes: 2,
        handshake_timeout: Duration::from_millis(150),
        ..host_config(None)
    })
    .await;
    let link = connected(&backend, &relay).await;
    let mut first = http_tunnel(&link).await;
    let _ = ok_h1(&mut first, "GET", "/v2/health", None).await;

    let client = RelayClient::from_link(&link, ClientConfig::default()).unwrap();
    let stalled_a = tokio::time::timeout(WAIT, client.open(link.room_id()))
        .await
        .unwrap()
        .unwrap();
    let mut second = tokio::time::timeout(Duration::from_secs(5), http_tunnel(&link))
        .await
        .expect("serving slot free");
    let _ = ok_h1(&mut second, "GET", "/v2/health", None).await;
    drop(second);

    let stalled_b = tokio::time::timeout(WAIT, client.open(link.room_id()))
        .await
        .unwrap()
        .unwrap();
    let _ = ok_h1(&mut first, "GET", "/v2/health", None).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut third = http_tunnel(&link).await;
    let _ = ok_h1(&mut third, "GET", "/v2/health", None).await;
    assert_eq!(backend.relay.status().active_streams, 2);
    drop((stalled_a, stalled_b));
    relay.stop().await;
}
