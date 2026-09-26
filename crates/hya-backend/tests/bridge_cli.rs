//! `hya bridge`: the client side of the secure relay (docs/relay.md
//! "Connecting from a client"). The relay and a test backend run in this
//! process: the backend registers a room with `register_host`, answers every
//! data stream as the Noise responder (`NoiseStream::respond`), and serves a
//! tiny hyper HTTP app behind it. `hya bridge` itself is spawned as a
//! subprocess, like every other CLI test in this crate.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
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

const WAIT: Duration = Duration::from_secs(20);

// ---- relay and test backend ----

async fn start_relay() -> (SocketAddr, JoinHandle<()>) {
    let config = RelayServerConfig::new("127.0.0.1:0".parse().unwrap());
    let (addr, serve) = RelayServer::bind(config, std::future::pending())
        .await
        .unwrap();
    (addr, tokio::spawn(serve))
}

fn relay_address(relay: SocketAddr) -> RelayAddress {
    RelayAddress::new(false, "127.0.0.1", Some(relay.port()), "").unwrap()
}

/// What the test backend does with each tunnel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A hyper HTTP/1.1 app (health, echo, a streamed body, a WebSocket-style upgrade).
    Http,
    /// Answer a partial response, then end the stream without the Noise
    /// close record (a hop cutting the response short).
    Truncate,
    /// Answer with a record whose ciphertext was modified in flight.
    Tamper,
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
        RelayLink::new(
            relay_address(relay),
            self.room(),
            Transport::Auto,
            *self.noise.public(),
            *self.psk.as_bytes(),
        )
    }
}

struct Host {
    link: RelayLink,
    /// Lets `/stream` send its second event.
    release: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Register `seed`'s room on the relay and serve every incoming stream.
async fn start_host(relay: SocketAddr, seed: u8, mode: Mode) -> Host {
    let identity = Arc::new(Identity::new(seed));
    let client = RelayClient::new(relay_address(relay), ClientConfig::default()).unwrap();
    let mut control = client.host().await.unwrap();
    let open_token_hash =
        hya_relay::keys::OpenToken::derive(&identity.psk, &identity.room()).hash();
    let room = register_host(&mut control, &identity.key, &open_token_hash, WAIT)
        .await
        .unwrap();
    assert_eq!(room, identity.room());
    let link = identity.link(relay);
    let release = Arc::new(Notify::new());
    let shared = release.clone();
    let task = tokio::spawn(async move {
        while let Some(Ok(frame)) = control.next().await {
            let ProxyToHost {
                frame: Some(proxy_to_host::Frame::Incoming(incoming)),
            } = frame
            else {
                continue;
            };
            let client = client.clone();
            let identity = identity.clone();
            let room = room.clone();
            let release = shared.clone();
            tokio::spawn(async move {
                let Ok(leg) = client.accept(&incoming.stream_id).await else {
                    return;
                };
                let leg: ChunkTransport = Box::pin(Mangle {
                    inner: leg,
                    mode,
                    data_frames: 0,
                });
                let Ok(Ok(tunnel)) = timeout(
                    WAIT,
                    NoiseStream::respond(
                        leg,
                        &room,
                        &identity.noise,
                        &identity.psk,
                        TunnelConfig::default(),
                    ),
                )
                .await
                else {
                    return;
                };
                match mode {
                    Mode::Http => {
                        let service = service_fn(move |request| app(request, release.clone()));
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(tunnel), service)
                            .with_upgrades()
                            .await;
                    }
                    Mode::Truncate | Mode::Tamper => partial_response(tunnel).await,
                }
            });
        }
    });
    Host {
        link,
        release,
        task,
    }
}

/// Mangles the backend's outgoing data frames: `Tamper` flips a bit of the
/// first record after the handshake; `Truncate` drops the close record (an
/// encrypted empty record: just the 16-byte tag).
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
            let index = self.data_frames;
            self.data_frames += 1;
            match self.mode {
                Mode::Tamper if index == 1 => data[0] ^= 0x01,
                Mode::Truncate if index >= 1 && data.len() == 16 => return Ok(()),
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

/// Read a request head, answer part of a response, and end the direction.
async fn partial_response<T>(mut tunnel: T)
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
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
    tokio::time::sleep(Duration::from_secs(1)).await;
}

type AppBody = BoxBody<Bytes, Infallible>;

fn full(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Response<AppBody> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(body.into()).boxed())
        .unwrap()
}

async fn app(
    mut request: Request<Incoming>,
    release: Arc<Notify>,
) -> Result<Response<AppBody>, Infallible> {
    Ok(match request.uri().path() {
        "/v1/health" => full(
            StatusCode::OK,
            "application/json",
            r#"{"ok":true,"version":"test"}"#,
        ),
        "/echo" => {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            full(StatusCode::OK, "application/octet-stream", body)
        }
        "/stream" => {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(Frame::data(Bytes::from("data: one\n\n")))).await;
                release.notified().await;
                let _ = tx.send(Ok(Frame::data(Bytes::from("data: two\n\n")))).await;
            });
            let body = StreamBody::new(tokio_stream::wrappers::ReceiverStream::new(rx));
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(BodyExt::boxed(body))
                .unwrap()
        }
        "/ws" => {
            let upgrade = hyper::upgrade::on(&mut request);
            tokio::spawn(async move {
                let Ok(upgraded) = upgrade.await else {
                    return;
                };
                let mut io = TokioIo::new(upgraded);
                let mut buf = [0u8; 1024];
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

// ---- the `hya bridge` process ----

struct BridgeProcess {
    child: Child,
    first_line: String,
    stderr: Arc<Mutex<String>>,
}

impl BridgeProcess {
    fn json(&self) -> Value {
        serde_json::from_str(&self.first_line)
            .unwrap_or_else(|error| panic!("{error}: {:?}", self.first_line))
    }

    fn url(&self) -> String {
        self.json()["url"].as_str().unwrap().to_owned()
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.child.id().unwrap()).unwrap()
    }
}

fn scratch_home() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hya-bridge-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn bridge_command(args: &[&str]) -> Command {
    let home = scratch_home();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("NO_COLOR", "1")
        .arg("bridge")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}

/// Spawn `hya bridge <args>`, write `stdin` (if any), and wait for the
/// first stdout line (the JSON line or the plain readiness line).
async fn spawn_bridge(
    mut command: Command,
    stdin: Option<String>,
    keep_stdin: bool,
) -> BridgeProcess {
    let mut child = command.spawn().unwrap();
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
        while let Ok(Some(line)) = err_lines.next_line().await {
            let mut text = keep.lock().unwrap();
            text.push_str(&line);
            text.push('\n');
        }
    });
    let mut out_lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first_line = timeout(WAIT, out_lines.next_line())
        .await
        .unwrap_or_else(|_| panic!("no readiness line; stderr: {}", stderr.lock().unwrap()))
        .unwrap()
        .unwrap_or_else(|| panic!("the bridge exited; stderr: {}", stderr.lock().unwrap()));
    tokio::spawn(async move { while let Ok(Some(_)) = out_lines.next_line().await {} });
    BridgeProcess {
        child,
        first_line,
        stderr,
    }
}

async fn bridge_json(link: &RelayLink) -> BridgeProcess {
    spawn_bridge(
        bridge_command(&["-", "--json"]),
        Some(format!("{}\n", link.to_secret_string())),
        false,
    )
    .await
}

/// The link's secret parts never appear in `text`.
fn assert_no_secret(text: &str, link: &RelayLink) {
    let secret = link.to_secret_string();
    let (_, fragment) = secret.split_once('#').unwrap();
    let (key, psk) = fragment.split_once('.').unwrap();
    assert!(!text.contains(fragment), "the link secret leaked: {text}");
    assert!(!text.contains(key), "the server key leaked: {text}");
    assert!(!text.contains(psk), "the PSK leaked: {text}");
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(WAIT).build().unwrap()
}

async fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    timeout(WAIT, child.wait())
        .await
        .expect("the bridge exits")
        .unwrap()
}

/// Write `request` on a new connection to the bridge and read until the
/// connection ends: the bytes, or the error that ended it.
async fn raw(url: &str, request: &[u8]) -> (Vec<u8>, Option<std::io::Error>) {
    let addr = url.trim_start_matches("http://").trim_end_matches('/');
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    tcp.write_all(request).await.unwrap();
    let mut received = Vec::new();
    let result = timeout(WAIT, tcp.read_to_end(&mut received))
        .await
        .expect("the bridge ends the connection");
    (received, result.err())
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_requests_cross_the_bridge_and_the_json_line_names_the_remote() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 1, Mode::Http).await;
    let mut bridge = bridge_json(&host.link).await;

    let json = bridge.json();
    let url = bridge.url();
    assert!(url.starts_with("http://127.0.0.1:"), "{json}");
    assert_eq!(json["room"], host.link.room_id().as_str());
    assert_eq!(
        json["proxy"],
        format!("hya+insecure://127.0.0.1:{}", relay.port())
    );
    assert_eq!(
        json["label"],
        format!("remote: 127.0.0.1:{}/{}", relay.port(), host.link.room_id())
    );

    let client = http();
    let health: Value = client
        .get(format!("{url}/v1/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["ok"], true);
    // Keep-alive reuse and a request body.
    for payload in ["first body", "second body"] {
        let echoed = client
            .post(format!("{url}/echo"))
            .body(payload)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(echoed, payload);
    }

    // SIGTERM stops it cleanly.
    // SAFETY: `kill` has no memory-safety preconditions; the pid is our child.
    unsafe {
        libc::kill(bridge.pid(), libc::SIGTERM);
    }
    let status = wait_exit(&mut bridge.child).await;
    assert!(status.success(), "{status:?}\n{}", bridge.stderr());
    let stderr = bridge.stderr();
    assert!(stderr.contains("binding"), "{stderr}");
    assert_no_secret(&stderr, &host.link);
    assert_no_secret(&bridge.first_line, &host.link);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_streamed_response_arrives_as_it_is_written() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 2, Mode::Http).await;
    let bridge = bridge_json(&host.link).await;
    let response = http()
        .get(format!("{}/stream", bridge.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut body = response.bytes_stream();
    let first = timeout(WAIT, body.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(&first[..], b"data: one\n\n");
    // The second event is written only now: the first was not buffered.
    host.release.notify_one();
    let mut rest = Vec::new();
    while let Some(chunk) = timeout(WAIT, body.next()).await.unwrap() {
        rest.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(rest, b"data: two\n\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_websocket_upgrade_crosses_the_bridge() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 3, Mode::Http).await;
    let bridge = bridge_json(&host.link).await;
    let addr = bridge.url().trim_start_matches("http://").to_owned();
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    tcp.write_all(
        b"GET /ws HTTP/1.1\r\nHost: bridge\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
          Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        timeout(WAIT, tcp.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    for message in [&b"raw frame one"[..], b"and two"] {
        tcp.write_all(message).await.unwrap();
        let mut back = vec![0u8; message.len()];
        timeout(WAIT, tcp.read_exact(&mut back))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(back, message);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offline_backend_answers_503_json_and_other_bytes_are_reset() {
    let (relay, _relay) = start_relay().await;
    // Nobody registered this room.
    let link = Identity::new(4).link(relay);
    let bridge = bridge_json(&link).await;
    let response = http()
        .get(format!("{}/v1/health", bridge.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        serde_json::json!({"error": {"code": "unavailable", "message": "remote backend is offline"}})
    );
    // Not HTTP: the connection is reset, not answered.
    let (received, error) = raw(&bridge.url(), b"\x00\x01not http at all\r\n\r\n").await;
    assert!(received.is_empty(), "{received:?}");
    assert_eq!(
        error.map(|e| e.kind()),
        Some(std::io::ErrorKind::ConnectionReset)
    );
    let stderr = bridge.stderr();
    assert!(stderr.contains("offline"), "{stderr}");
    assert_no_secret(&stderr, &link);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_truncated_response_resets_the_client_connection() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 5, Mode::Truncate).await;
    let bridge = bridge_json(&host.link).await;
    let (_received, error) = raw(&bridge.url(), b"GET / HTTP/1.1\r\nHost: b\r\n\r\n").await;
    assert_eq!(
        error.map(|e| e.kind()),
        Some(std::io::ErrorKind::ConnectionReset),
        "a cut response must not look like a clean close"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tampered_response_resets_the_client_connection() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 6, Mode::Tamper).await;
    let bridge = bridge_json(&host.link).await;
    let (received, error) = raw(&bridge.url(), b"GET / HTTP/1.1\r\nHost: b\r\n\r\n").await;
    assert!(received.is_empty(), "no tampered plaintext: {received:?}");
    assert_eq!(
        error.map(|e| e.kind()),
        Some(std::io::ErrorKind::ConnectionReset)
    );
    let stderr = bridge.stderr();
    assert!(stderr.contains("integrity"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_loopback_listen_address_is_refused() {
    for listen in ["0.0.0.0:0", "192.0.2.1:0", "[::]:0"] {
        let mut command = bridge_command(&["-", "--listen", listen]);
        command.stdin(std::process::Stdio::null());
        let output = timeout(WAIT, command.output()).await.unwrap().unwrap();
        assert!(!output.status.success(), "{listen}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("loopback"), "{listen}: {stderr}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_link_comes_from_the_environment_and_listen_pins_the_port() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 7, Mode::Http).await;
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let listen = format!("127.0.0.1:{port}");
    let mut command = bridge_command(&["--listen", &listen]);
    command.env("HYA_RELAY_LINK", host.link.to_secret_string());
    let bridge = spawn_bridge(command, None, false).await;
    assert_eq!(
        bridge.first_line,
        format!("hya bridge listening on http://127.0.0.1:{port}")
    );
    let health = http()
        .get(format!("http://127.0.0.1:{port}/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_no_secret(&bridge.stderr(), &host.link);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exit_with_stdin_stops_the_bridge_when_its_parent_goes_away() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 8, Mode::Http).await;
    let mut bridge = spawn_bridge(
        bridge_command(&["-", "--json", "--exit-with-stdin"]),
        Some(format!("{}\n", host.link.to_secret_string())),
        true,
    )
    .await;
    assert!(bridge.url().starts_with("http://127.0.0.1:"));
    drop(bridge.child.stdin.take());
    let status = wait_exit(&mut bridge.child).await;
    assert!(status.success(), "{status:?}\n{}", bridge.stderr());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_link_fails_at_start_without_printing_the_secret() {
    let (relay, _relay) = start_relay().await;
    let host = start_host(relay, 9, Mode::Http).await;
    // Right room, wrong PSK: the backend rejects the handshake.
    let wrong = RelayLink::new(
        host.link.address().clone(),
        host.link.room_id().clone(),
        Transport::Auto,
        *host.link.server_key(),
        [0x42; 32],
    );
    let secret = wrong.to_secret_string();
    // A link given as an argument works but earns a warning (process listings).
    let mut command = bridge_command(&[&secret]);
    command.stdin(std::process::Stdio::null());
    let output = timeout(WAIT, command.output()).await.unwrap().unwrap();
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rejected"), "{stderr}");
    assert!(stderr.contains("process list"), "{stderr}");
    assert_no_secret(&stderr, &wrong);
    assert_no_secret(&String::from_utf8_lossy(&output.stdout), &wrong);
}
