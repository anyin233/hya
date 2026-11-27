//! Real-process integration tests for `hya bridge` against an in-process relay
//! and Noise host. Pure listen/link/token/envelope validation lives in `src/bridge.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use futures::{Sink, Stream, StreamExt};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hya_relay::client::{ClientConfig, RelayClient, register_host};
use hya_relay::keys::{Psk, StaticKeypair};
use hya_relay::link::{RelayAddress, RelayLink, RoomId, Transport};
use hya_relay::proto::{Chunk, ProxyToHost, chunk, proxy_to_host};
use hya_relay::server::{RelayServer, RelayServerConfig};
use hya_relay::transport::{ChunkTransport, TransportError};
use hya_relay::tunnel::{NoiseStream, TunnelConfig};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::common::Scratch;

const WAIT: Duration = Duration::from_secs(15);
const TOKEN_HEADER: &str = "x-hya-bridge-token";
const OFFLINE: &str = "remote backend is offline, or the relay link was rotated or is wrong (ask for a new link: `hya serve relay link`)";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Http,
    Truncate,
    Tamper,
}

async fn start_relay() -> (SocketAddr, JoinHandle<()>) {
    let (addr, serve) = RelayServer::bind(
        RelayServerConfig::new("127.0.0.1:0".parse().unwrap()),
        std::future::pending(),
    )
    .await
    .unwrap();
    (addr, tokio::spawn(serve))
}

struct Identity {
    key: SigningKey,
    noise: StaticKeypair,
    psk: Psk,
}

impl Identity {
    fn new(seed: u8) -> Self {
        Self {
            key: SigningKey::from_bytes(&[seed; 32]),
            noise: StaticKeypair::from_secret([seed.wrapping_add(100); 32]).unwrap(),
            psk: Psk::from_bytes([seed.wrapping_add(200); 32]),
        }
    }
    fn room(&self) -> RoomId {
        RoomId::from_ed25519(self.key.verifying_key().as_bytes())
    }
    fn link(&self, relay: SocketAddr) -> RelayLink {
        let addr = RelayAddress::new(false, "127.0.0.1", Some(relay.port()), "").unwrap();
        RelayLink::new(
            addr,
            self.room(),
            Transport::Auto,
            *self.noise.public(),
            *self.psk.as_bytes(),
        )
    }
}

struct Host {
    link: RelayLink,
    incoming: Arc<AtomicUsize>,
    release: Arc<Notify>,
    task: JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_host(relay: SocketAddr, seed: u8, mode: Mode) -> Host {
    let id = Arc::new(Identity::new(seed));
    let addr = RelayAddress::new(false, "127.0.0.1", Some(relay.port()), "").unwrap();
    let client = RelayClient::new(addr, ClientConfig::default()).unwrap();
    let mut control = client.host().await.unwrap();
    let hash = hya_relay::keys::OpenToken::derive(&id.psk, &id.room()).hash();
    let room = register_host(&mut control, &id.key, &hash, WAIT)
        .await
        .unwrap()
        .room()
        .clone();
    let link = id.link(relay);
    let release = Arc::new(Notify::new());
    let incoming = Arc::new(AtomicUsize::new(0));
    let (rel, cnt) = (release.clone(), incoming.clone());
    let task = tokio::spawn(async move {
        while let Some(Ok(ProxyToHost {
            frame: Some(proxy_to_host::Frame::Incoming(inc)),
        })) = control.next().await
        {
            cnt.fetch_add(1, Ordering::SeqCst);
            let (client, id, room, rel) = (client.clone(), id.clone(), room.clone(), rel.clone());
            tokio::spawn(async move {
                let Ok(leg) = client.accept(&inc.stream_id).await else {
                    return;
                };
                let leg: ChunkTransport = Box::pin(Mangle {
                    inner: leg,
                    mode,
                    data_frames: 0,
                });
                let Ok(Ok(tunnel)) = timeout(
                    WAIT,
                    NoiseStream::respond(leg, &room, &id.noise, &id.psk, TunnelConfig::default()),
                )
                .await
                else {
                    return;
                };
                match mode {
                    Mode::Http => {
                        let svc = service_fn(move |req| app(req, rel.clone()));
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(tunnel), svc)
                            .with_upgrades()
                            .await;
                    }
                    Mode::Truncate | Mode::Tamper => partial_response(tunnel, rel).await,
                }
            });
        }
    });
    Host {
        link,
        incoming,
        release,
        task,
    }
}

struct Mangle {
    inner: ChunkTransport,
    mode: Mode,
    data_frames: usize,
}
impl Sink<Chunk> for Mangle {
    type Error = TransportError;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.as_mut().poll_ready(cx)
    }
    fn start_send(mut self: Pin<&mut Self>, mut item: Chunk) -> Result<(), Self::Error> {
        if let Some(chunk::Frame::Data(data)) = &mut item.frame {
            let i = self.data_frames;
            self.data_frames += 1;
            match self.mode {
                Mode::Tamper if i == 1 => data[0] ^= 0x01,
                Mode::Truncate if i >= 1 && data.len() == 16 => return Ok(()),
                _ => {}
            }
        }
        self.inner.as_mut().start_send(item)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.as_mut().poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.as_mut().poll_close(cx)
    }
}
impl Stream for Mangle {
    type Item = Result<Chunk, TransportError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

async fn partial_response<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut tunnel: T,
    release: Arc<Notify>,
) {
    let (mut head, mut buf) = (Vec::new(), [0u8; 1024]);
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match tunnel.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let _ = tunnel
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1000\r\n\r\npartial")
        .await;
    let _ = tunnel.flush().await;
    let _ = tunnel.shutdown().await;
    let _ = timeout(Duration::from_millis(500), release.notified()).await;
}

type AppBody = BoxBody<Bytes, Infallible>;
fn full(status: StatusCode, ct: &str, body: impl Into<Bytes>) -> Response<AppBody> {
    Response::builder()
        .status(status)
        .header("content-type", ct)
        .body(Full::new(body.into()).boxed())
        .unwrap()
}

async fn app(
    mut req: Request<Incoming>,
    release: Arc<Notify>,
) -> Result<Response<AppBody>, Infallible> {
    Ok(match req.uri().path() {
        "/v2/health" => full(
            StatusCode::OK,
            "application/json",
            r#"{"ok":true,"version":"test"}"#,
        ),
        "/echo" => full(
            StatusCode::OK,
            "application/octet-stream",
            req.into_body().collect().await.unwrap().to_bytes(),
        ),
        "/headers" => {
            let text = req.headers().iter().fold(String::new(), |mut s, (k, v)| {
                s.push_str(&format!("{k}: {}\n", v.to_str().unwrap_or("?")));
                s
            });
            full(StatusCode::OK, "text/plain", text)
        }
        "/stream" => {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(Frame::data(Bytes::from("data: one\n\n")))).await;
                release.notified().await;
                let _ = tx.send(Ok(Frame::data(Bytes::from("data: two\n\n")))).await;
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(BodyExt::boxed(StreamBody::new(
                    tokio_stream::wrappers::ReceiverStream::new(rx),
                )))
                .unwrap()
        }
        "/ws" => {
            let upgrade = hyper::upgrade::on(&mut req);
            tokio::spawn(async move {
                let Ok(up) = upgrade.await else { return };
                let (mut io, mut buf) = (TokioIo::new(up), [0u8; 1024]);
                while let Ok(n) = io.read(&mut buf).await {
                    if n == 0 || io.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
            Response::builder()
                .status(StatusCode::SWITCHING_PROTOCOLS)
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .body(Full::new(Bytes::new()).boxed())
                .unwrap()
        }
        _ => full(StatusCode::NOT_FOUND, "text/plain", "not found"),
    })
}

struct BridgeProcess {
    child: Child,
    first_line: String,
    token: String,
    stderr: Arc<Mutex<String>>,
    _scratch: Scratch,
}

impl BridgeProcess {
    fn json(&self) -> Value {
        serde_json::from_str(&self.first_line).unwrap()
    }
    fn url(&self) -> String {
        self.json()["url"].as_str().unwrap().to_owned()
    }
    fn client(&self) -> reqwest::Client {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(TOKEN_HEADER, self.token.parse().unwrap());
        reqwest::Client::builder()
            .timeout(WAIT)
            .default_headers(h)
            .build()
            .unwrap()
    }
    fn authed(&self, head: &str) -> Vec<u8> {
        let (line, rest) = head.split_once("\r\n").unwrap();
        format!("{line}\r\n{TOKEN_HEADER}: {}\r\n{rest}", self.token).into_bytes()
    }
    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
    async fn stderr_containing(&self, needle: &str) -> String {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let s = self.stderr();
            if s.contains(needle) || tokio::time::Instant::now() >= deadline {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn bridge_command(args: &[&str]) -> (Command, Scratch) {
    let scratch = Scratch::new("hya-bridge").unwrap();
    let mut cmd = Command::new(crate::common::hya_bin());
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &scratch.0)
        .env("XDG_STATE_HOME", scratch.0.join("state"))
        .env("XDG_CONFIG_HOME", scratch.0.join("config"))
        .env("NO_COLOR", "1")
        .arg("bridge")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    (cmd, scratch)
}

async fn spawn_bridge(
    mut cmd: Command,
    scratch: Scratch,
    stdin: Option<String>,
    keep_stdin: bool,
) -> BridgeProcess {
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        pipe.write_all(text.as_bytes()).await.unwrap();
        pipe.flush().await.unwrap();
    }
    if keep_stdin {
        child.stdin = Some(pipe);
    } else {
        drop(pipe);
    }
    let stderr = Arc::new(Mutex::new(String::new()));
    let keep = stderr.clone();
    let mut err_lines = BufReader::new(child.stderr.take().unwrap()).lines();
    tokio::spawn(async move {
        while let Ok(Some(l)) = err_lines.next_line().await {
            let mut t = keep.lock().unwrap();
            t.push_str(&l);
            t.push('\n');
        }
    });
    let mut out_lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first_line = timeout(WAIT, out_lines.next_line())
        .await
        .unwrap_or_else(|_| panic!("no readiness line; stderr: {}", stderr.lock().unwrap()))
        .unwrap()
        .unwrap_or_else(|| panic!("bridge exited; stderr: {}", stderr.lock().unwrap()));
    let token = if let Ok(json) = serde_json::from_str::<Value>(&first_line) {
        json["token"].as_str().unwrap_or_default().to_owned()
    } else {
        timeout(WAIT, out_lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap_or_default()
            .strip_prefix("hya bridge token ")
            .unwrap()
            .to_owned()
    };
    tokio::spawn(async move { while let Ok(Some(_)) = out_lines.next_line().await {} });
    BridgeProcess {
        child,
        first_line,
        token,
        stderr,
        _scratch: scratch,
    }
}

async fn bridge_json(link: &RelayLink) -> BridgeProcess {
    let (cmd, scratch) = bridge_command(&["-", "--json"]);
    spawn_bridge(
        cmd,
        scratch,
        Some(format!("{}\n", link.to_secret_string())),
        false,
    )
    .await
}

fn assert_no_secret(text: &str, link: &RelayLink) {
    let secret = link.to_secret_string();
    let (_, frag) = secret.split_once('#').unwrap();
    let (key, psk) = frag.split_once('.').unwrap();
    assert!(
        !text.contains(frag) && !text.contains(key) && !text.contains(psk),
        "secret leaked: {text}"
    );
}

async fn raw(url: &str, req: &[u8]) -> (Vec<u8>, Option<std::io::Error>) {
    let mut tcp = TcpStream::connect(url.trim_start_matches("http://").trim_end_matches('/'))
        .await
        .unwrap();
    tcp.write_all(req).await.unwrap();
    let mut received = Vec::new();
    let res = timeout(WAIT, tcp.read_to_end(&mut received))
        .await
        .expect("bridge ends connection");
    (received, res.err())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_sse_websocket_and_sigterm_cross_the_bridge() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 1, Mode::Http).await;
    let mut bridge = bridge_json(&host.link).await;
    let (json, url) = (bridge.json(), bridge.url());
    assert!(url.starts_with("http://127.0.0.1:") && json["room"] == host.link.room_id().as_str());
    assert_eq!(
        json["label"],
        format!("remote: 127.0.0.1:{}/{}", relay.port(), host.link.room_id())
    );

    let client = bridge.client();
    assert_eq!(
        client
            .get(format!("{url}/v2/health"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["ok"],
        true
    );
    for payload in ["first body", "second body"] {
        assert_eq!(
            client
                .post(format!("{url}/echo"))
                .body(payload)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            payload
        );
    }

    // Unbuffered SSE streaming.
    let resp = client.get(format!("{url}/stream")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = resp.bytes_stream();
    assert_eq!(
        &timeout(WAIT, body.next()).await.unwrap().unwrap().unwrap()[..],
        b"data: one\n\n"
    );
    host.release.notify_one();
    let mut rest = Vec::new();
    while let Some(chunk) = timeout(WAIT, body.next()).await.unwrap() {
        rest.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(rest, b"data: two\n\n");

    // WebSocket upgrade.
    let mut tcp = TcpStream::connect(url.trim_start_matches("http://"))
        .await
        .unwrap();
    tcp.write_all(&bridge.authed(
        "GET /ws HTTP/1.1\r\nHost: bridge\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )).await.unwrap();
    let (mut head, mut b) = (Vec::new(), [0u8; 1]);
    while !head.ends_with(b"\r\n\r\n") {
        timeout(WAIT, tcp.read_exact(&mut b))
            .await
            .unwrap()
            .unwrap();
        head.push(b[0]);
    }
    assert!(String::from_utf8(head).unwrap().starts_with("HTTP/1.1 101"));
    tcp.write_all(b"raw frame").await.unwrap();
    let mut back = [0u8; 9];
    timeout(WAIT, tcp.read_exact(&mut back))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&back, b"raw frame");
    drop(tcp);

    // SAFETY: `kill` targets our unreaped child process.
    unsafe {
        libc::kill(
            i32::try_from(bridge.child.id().unwrap()).unwrap(),
            libc::SIGTERM,
        )
    };
    assert!(
        timeout(WAIT, bridge.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_no_secret(&bridge.stderr(), &host.link);
    assert_no_secret(&bridge.first_line, &host.link);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_requests_are_refused_and_token_is_stripped() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 10, Mode::Http).await;
    let bridge = bridge_json(&host.link).await;
    assert_eq!(bridge.token.len(), 64);
    let (before, url) = (host.incoming.load(Ordering::SeqCst), bridge.url());
    let http = reqwest::Client::builder().timeout(WAIT).build().unwrap();
    for req in [
        http.get(format!("{url}/v2/health")),
        http.get(format!("{url}/v2/health"))
            .header(TOKEN_HEADER, "0".repeat(64)),
        http.get(format!("{url}/v2/health"))
            .header("authorization", format!("Bearer {}", bridge.token)),
    ] {
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), 401);
        assert_eq!(
            resp.json::<Value>().await.unwrap()["error"]["code"],
            "unauthenticated"
        );
    }
    assert!(
        raw(&url, b"\x16\x03\x01 not http\r\n\r\n")
            .await
            .0
            .is_empty()
    );
    assert_eq!(host.incoming.load(Ordering::SeqCst), before);

    assert_eq!(
        bridge
            .client()
            .get(format!("{url}/v2/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(host.incoming.load(Ordering::SeqCst), before + 1);
    let (received, _) = raw(
        &url,
        &bridge.authed("POST /headers HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    )
    .await;
    let text = String::from_utf8_lossy(&received).to_ascii_lowercase();
    assert!(
        text.starts_with("http/1.1 200") && !text.contains(TOKEN_HEADER),
        "{text}"
    );
    let stderr = bridge.stderr_containing("without the bridge token").await;
    assert!(stderr.contains("without the bridge token") && !stderr.contains(&bridge.token));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_rotated_and_rejected_links_fail_without_leaking_secrets() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 9, Mode::Http).await;

    let offline_link = Identity::new(4).link(relay);
    let offline = bridge_json(&offline_link).await;
    let resp = offline
        .client()
        .get(format!("{}/v2/health", offline.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        serde_json::json!({"error": {"code": "unavailable", "message": OFFLINE}})
    );
    let (bytes, err) = raw(&offline.url(), b"\x00\x01not http\r\n\r\n").await;
    assert!(bytes.is_empty() && err.map(|e| e.kind()) == Some(std::io::ErrorKind::ConnectionReset));
    assert_no_secret(&offline.stderr_containing("offline").await, &offline_link);

    let rotated = RelayLink::new(
        host.link.address().clone(),
        host.link.room_id().clone(),
        Transport::Auto,
        *host.link.server_key(),
        [0x42; 32],
    );
    let (cmd, scratch) = bridge_command(&["--json", &rotated.to_secret_string()]);
    let rot = spawn_bridge(cmd, scratch, None, false).await;
    assert_eq!(
        rot.client()
            .get(format!("{}/v2/health", rot.url()))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(host.incoming.load(Ordering::SeqCst), 0);
    let stderr = rot.stderr_containing("rotated or is wrong").await;
    assert!(stderr.contains("process list"));
    assert_no_secret(&stderr, &rotated);

    let wrong = RelayLink::new(
        host.link.address().clone(),
        host.link.room_id().clone(),
        Transport::Auto,
        *Identity::new(99).noise.public(),
        *host.link.psk(),
    );
    let (mut cmd, _scratch) = bridge_command(&[&wrong.to_secret_string()]);
    cmd.stdin(std::process::Stdio::null());
    let output = timeout(WAIT, cmd.output()).await.unwrap().unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success() && err.contains("rejected") && err.contains("process list"));
    assert_no_secret(&err, &wrong);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn truncated_tampered_env_listen_and_exit_with_stdin() {
    let (relay, _relay) = start_relay().await;
    let trunc_host = start_host(relay, 5, Mode::Truncate).await;
    let trunc = bridge_json(&trunc_host.link).await;
    let (_, err) = raw(
        &trunc.url(),
        &trunc.authed("GET / HTTP/1.1\r\nHost: b\r\n\r\n"),
    )
    .await;
    trunc_host.release.notify_waiters();
    assert_eq!(
        err.map(|e| e.kind()),
        Some(std::io::ErrorKind::ConnectionReset)
    );

    let tamp_host = start_host(relay, 6, Mode::Tamper).await;
    let tamp = bridge_json(&tamp_host.link).await;
    let (bytes, err) = raw(
        &tamp.url(),
        &tamp.authed("GET / HTTP/1.1\r\nHost: b\r\n\r\n"),
    )
    .await;
    tamp_host.release.notify_waiters();
    assert!(bytes.is_empty() && err.map(|e| e.kind()) == Some(std::io::ErrorKind::ConnectionReset));
    assert!(
        tamp.stderr_containing("integrity")
            .await
            .contains("integrity")
    );

    let host = start_host(relay, 7, Mode::Http).await;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    let (mut cmd, scratch) = bridge_command(&["--listen", &listen, "--exit-with-stdin"]);
    cmd.env("HYA_RELAY_LINK", host.link.to_secret_string());
    let mut bridge = spawn_bridge(cmd, scratch, None, true).await;
    assert_eq!(
        bridge.first_line,
        format!("hya bridge listening on http://127.0.0.1:{port}")
    );
    assert_eq!(
        bridge
            .client()
            .get(format!("http://127.0.0.1:{port}/v2/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    drop(bridge.child.stdin.take());
    assert!(
        timeout(WAIT, bridge.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
