//! T2.35–T2.37 — the secure relay golden path, process level (ADR-0025,
//! docs/relay.md): a real `hya proxy`, a real `hya serve --relay` backed by
//! FakeLlm, and a real `hya bridge`, with every client request going through
//! the bridge's loopback URL only.
//!
//! An in-test TCP hop sits in front of the proxy and records every byte in
//! both directions; the backend's `--relay` URL and the link both point at
//! it, so it sees exactly what the proxy (or any hop in front of it) sees.
//! The recording must never contain a prompt, a file's contents, a shell's
//! output, or the link's pre-shared key.
//!
//! - T2.35 (gRPC) and T2.36 (WebSocket): a plaintext relay pinned to one
//!   binding, the whole path with the capture assertions: health and
//!   bootstrap, a remotely created two-root Project, a prompt whose tool
//!   reads a file in the second root (no ask) and one outside the roots
//!   (an `ExternalDirectory` ask answered over the bridge), the session SSE
//!   stream, a PTY WebSocket, a temporary session, `RelayControl` refused
//!   from relay origin, key rotation (the running bridge is rejected, a new
//!   bridge with the new link works), and shutdown (`serverStopping` on the
//!   bridge-side SSE stream, then `503 remote backend is offline`).
//! - T2.37: TLS to the proxy (`hya://` link, private CA, `t=auto`): health,
//!   a session, a prompt, and the same capture assertions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures::{SinkExt as _, StreamExt as _};
use hya_client::Client;
use hya_e2e::{E2eEnv, E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio_tungstenite::tungstenite::Message;

/// Upper bound for every wait in this file; each wait polls and returns as
/// soon as its condition holds.
const WAIT: Duration = Duration::from_secs(20);

const PROMPT_MARKER: &str = "RELAY-PROMPT-7c1e93";
const ROOT_B_MARKER: &str = "RELAY-ROOT-B-CONTENT-4d2f18";
const OUTSIDE_MARKER: &str = "RELAY-OUTSIDE-CONTENT-a90b66";
const FINAL_MARKER: &str = "RELAY-FINAL-ANSWER-35e0c2";
/// The PTY is sent `printf '%s-%s\n' RELAYPTY 51f7ac`, so this string only
/// ever exists in the shell's output, never in the input.
const PTY_MARKER: &str = "RELAYPTY-51f7ac";

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("hya-relay-e2e-{}-{label}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir.canonicalize().expect("canonical fixture dir")
}

fn hya_bin() -> PathBuf {
    hya_e2e::default_backend_bin()
}

/// Print a child's stderr lines with `tag`.
fn echo_stderr(child: &mut Child, tag: &'static str) {
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[{tag}] {line}");
            }
        });
    }
}

/// First stdout line of `child`, within [`WAIT`].
async fn first_line(child: &mut Child, what: &str) -> String {
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(WAIT, lines.next_line())
        .await
        .unwrap_or_else(|_| panic!("{what}: no readiness line within {WAIT:?}"))
        .expect("read stdout")
        .unwrap_or_else(|| panic!("{what}: exited before its readiness line"));
    // Keep draining so the child never blocks on a full pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    line
}

// ---- hya proxy ----

struct Proxy {
    addr: SocketAddr,
    _child: Child,
}

impl Proxy {
    async fn start(tls: Option<&Tls>) -> Self {
        let mut command = Command::new(hya_bin());
        command.args(["proxy", "--host", "127.0.0.1", "--port", "0"]);
        if let Some(tls) = tls {
            command
                .arg("--tls-cert")
                .arg(&tls.cert)
                .arg("--tls-key")
                .arg(&tls.key);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn hya proxy");
        echo_stderr(&mut child, "proxy");
        let line = first_line(&mut child, "hya proxy").await;
        let scheme = if tls.is_some() { "https" } else { "http" };
        let url = line
            .strip_prefix(&format!("hya proxy listening on {scheme}://"))
            .unwrap_or_else(|| panic!("unexpected proxy readiness line: {line}"));
        Self {
            addr: url.parse().expect("proxy socket address"),
            _child: child,
        }
    }
}

// ---- the capture hop ----

/// The bytes of one connection in one direction.
type Recording = Arc<Mutex<Vec<u8>>>;

/// A transparent TCP forwarder in front of the proxy that keeps a copy of
/// every byte, one buffer per connection and direction (so a marker can
/// never be missed by interleaving with another connection).
struct Capture {
    addr: SocketAddr,
    buffers: Arc<Mutex<Vec<Recording>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Capture {
    async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind capture");
        let addr = listener.local_addr().expect("capture addr");
        let buffers: Arc<Mutex<Vec<Recording>>> = Arc::default();
        let shared = Arc::clone(&buffers);
        let task = tokio::spawn(async move {
            while let Ok((inbound, _)) = listener.accept().await {
                let Ok(outbound) = TcpStream::connect(upstream).await else {
                    continue;
                };
                let (in_read, in_write) = inbound.into_split();
                let (out_read, out_write) = outbound.into_split();
                let up = Recording::default();
                let down = Recording::default();
                shared
                    .lock()
                    .unwrap()
                    .extend([Arc::clone(&up), Arc::clone(&down)]);
                tokio::spawn(pump(in_read, out_write, up));
                tokio::spawn(pump(out_read, in_write, down));
            }
        });
        Self {
            addr,
            buffers,
            task,
        }
    }

    fn snapshot(&self) -> Vec<Vec<u8>> {
        self.buffers
            .lock()
            .unwrap()
            .iter()
            .map(|buffer| buffer.lock().unwrap().clone())
            .collect()
    }

    /// Assert the recording is real relay traffic and holds none of
    /// `secrets`. On a plaintext hop, `room` (which the proxy must see to
    /// route) is the positive control that the search does find plain bytes.
    fn assert_blind(&self, room: Option<&str>, secrets: &[(String, Vec<u8>)]) {
        let buffers = self.snapshot();
        if let Some(room) = room {
            assert!(
                buffers
                    .iter()
                    .any(|buffer| buffer.windows(room.len()).any(|w| w == room.as_bytes())),
                "control: the capture must see the room id the proxy routes on"
            );
        }
        let total: usize = buffers.iter().map(Vec::len).sum();
        assert!(
            buffers.len() >= 4 && total > 4096,
            "the capture hop must carry the relay traffic (connections×2={}, bytes={total})",
            buffers.len()
        );
        for (name, needle) in secrets {
            assert!(!needle.is_empty());
            for buffer in &buffers {
                assert!(
                    !buffer.windows(needle.len()).any(|window| window == needle),
                    "the bytes the proxy sees contain {name}"
                );
            }
        }
    }
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    record: Recording,
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                record.lock().unwrap().extend_from_slice(&buf[..n]);
                if to.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.shutdown().await;
}

// ---- TLS material ----

struct Tls {
    dir: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

impl Tls {
    fn new() -> Self {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("self-signed certificate");
        let dir = fixture_dir("tls");
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        std::fs::write(&cert, certified.cert.pem()).unwrap();
        std::fs::write(&key, certified.key_pair.serialize_pem()).unwrap();
        Self { dir, cert, key }
    }
}

impl Drop for Tls {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---- hya bridge ----

struct Bridge {
    url: String,
    room: String,
    _stdin: ChildStdin,
    _child: Child,
}

impl Bridge {
    /// `hya bridge - --json --exit-with-stdin`, the link on stdin (never in argv).
    async fn start(link: &str, ca: Option<&Path>) -> Self {
        let mut command = Command::new(hya_bin());
        command.args(["bridge", "-", "--json", "--exit-with-stdin"]);
        if let Some(ca) = ca {
            command.arg("--relay-ca").arg(ca);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn hya bridge");
        echo_stderr(&mut child, "bridge");
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin
            .write_all(format!("{link}\n").as_bytes())
            .await
            .expect("write the link");
        stdin.flush().await.expect("flush the link");
        let line = first_line(&mut child, "hya bridge").await;
        let ready: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("bridge readiness is JSON ({e}): {line}"));
        let url = ready["url"].as_str().expect("bridge url").to_owned();
        assert!(url.starts_with("http://127.0.0.1:"), "{ready}");
        assert!(
            !line.contains('#'),
            "the readiness line never holds the secret"
        );
        Self {
            url,
            room: ready["room"].as_str().expect("bridge room").to_owned(),
            _stdin: stdin,
            _child: child,
        }
    }
}

// ---- helpers over the backend ----

/// The backend's link, from its one `hya relay link: ` stderr line.
fn link_of(env: &E2eEnv) -> String {
    let line = tokio::task::block_in_place(|| {
        env.backend
            .wait_stderr_line("hya relay link: ", WAIT)
            .expect("hya serve --relay prints its link")
    });
    line.strip_prefix("hya relay link: ").unwrap().to_owned()
}

/// `hya --db <db> serve relay <args>` on the backend's loopback; stdout JSON.
fn relay_cli(env: &E2eEnv, args: &[&str]) -> Value {
    let db = env.backend.db.display().to_string();
    let mut argv = vec!["--db", db.as_str(), "serve", "relay"];
    argv.extend_from_slice(args);
    let output = tokio::task::block_in_place(|| env.backend.cli(&argv)).expect("hya serve relay");
    assert!(
        output.status.success(),
        "hya serve relay {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("hya serve relay --json output")
}

async fn wait_relay_connected(env: &E2eEnv) {
    let deadline = Instant::now() + WAIT;
    loop {
        let status = relay_cli(env, &["status", "--json"]);
        if status["state"] == "RELAY_STATE_CONNECTED" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the backend never joined the relay: {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Point every client helper of `env` (typed client and raw JSON) at `url`.
fn route_through(env: &mut E2eEnv, url: &str) {
    env.backend.url = url.to_owned();
    env.client = Client::new(url.to_owned());
}

/// The link's secret parts as needles: the fragment, the PSK as base64url,
/// and the raw PSK bytes.
fn link_secrets(link: &str) -> Vec<(String, Vec<u8>)> {
    let fragment = link.split_once('#').expect("link fragment").1;
    let psk = fragment.split_once('.').expect("key.psk").1;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(psk)
        .expect("psk is unpadded base64url");
    assert_eq!(raw.len(), 32);
    vec![
        ("the link fragment".into(), fragment.as_bytes().to_vec()),
        ("the PSK (base64url)".into(), psk.as_bytes().to_vec()),
        ("the PSK (raw)".into(), raw),
    ]
}

/// A plain GET on a fresh connection (no pooling): status and body JSON.
async fn fresh_get(url: &str) -> (u16, Value) {
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            (status, serde_json::from_str(&text).unwrap_or(json!(text)))
        }
        Err(error) => (0, json!(error.to_string())),
    }
}

/// Poll `GET url` until `accept(status, body)` holds.
async fn wait_get(url: &str, what: &str, accept: impl Fn(u16, &Value) -> bool) -> (u16, Value) {
    let deadline = Instant::now() + WAIT;
    loop {
        let (status, body) = fresh_get(url).await;
        if accept(status, &body) {
            return (status, body);
        }
        assert!(
            Instant::now() < deadline,
            "{what}: last answer {status} {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A live SSE stream through the bridge; frames are collected in the background.
struct Sse {
    frames: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Sse {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Sse {
    /// Open `url` and return once the response head arrived (the server
    /// subscribes before it answers, so no later frame is missed).
    async fn open(url: &str) -> Self {
        let response = reqwest::Client::new()
            .get(url)
            .header("accept", "text/event-stream")
            .send()
            .await
            .expect("open SSE through the bridge");
        assert_eq!(response.status(), 200, "SSE {url}");
        let frames: Arc<Mutex<Vec<Value>>> = Arc::default();
        let sink = Arc::clone(&frames);
        let task = tokio::spawn(async move {
            let mut body = response.bytes_stream();
            let mut pending = String::new();
            while let Some(Ok(chunk)) = body.next().await {
                pending.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = pending.find("\n\n") {
                    let event: String = pending.drain(..end + 2).collect();
                    for line in event.lines() {
                        if let Some(data) = line.strip_prefix("data:")
                            && let Ok(frame) = serde_json::from_str::<Value>(data.trim())
                        {
                            sink.lock().unwrap().push(frame);
                        }
                    }
                }
            }
        });
        Self { frames, task }
    }

    fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap().clone()
    }

    async fn wait_for(&self, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let frames = self.frames();
            if done(&frames) {
                return frames;
            }
            assert!(
                Instant::now() < deadline,
                "SSE never showed {what}; frames: {frames:#?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn post(url: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(url)
        .json(&body)
        .send()
        .await
        .expect("POST through the bridge");
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(json!(text)))
}

/// Echo through a PTY opened over the bridge (create, connect token,
/// WebSocket upgrade); the marker comes back from the shell.
async fn pty_echo(bridge: &str, cwd: &Path) {
    let (status, pty) = post(
        &format!("{bridge}/v1/pty"),
        json!({"shell": "/bin/sh", "cwd": cwd.display().to_string()}),
    )
    .await;
    assert_eq!(status, 200, "{pty}");
    let id = pty["id"].as_str().expect("pty id");
    let (status, token) = post(&format!("{bridge}/v1/pty/{id}/connect-token"), json!({})).await;
    assert_eq!(status, 200, "{token}");
    let path = token["url"].as_str().expect("connect url");
    let ws = format!("{}{path}", bridge.replacen("http://", "ws://", 1));
    let (mut socket, response) = tokio_tungstenite::connect_async(ws)
        .await
        .expect("PTY WebSocket upgrade through the bridge");
    assert_eq!(response.status(), 101);
    let input =
        base64::engine::general_purpose::STANDARD.encode(b"printf '%s-%s\\n' RELAYPTY 51f7ac\n");
    socket
        .send(Message::Text(json!({"input": input}).to_string()))
        .await
        .expect("send PTY input");
    let seen = tokio::time::timeout(WAIT, async {
        let mut output = Vec::new();
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message
                && let Ok(frame) = serde_json::from_str::<Value>(&text)
                && let Some(bytes) = frame["output"].as_str()
            {
                output.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(bytes)
                        .unwrap_or_default(),
                );
                if String::from_utf8_lossy(&output).contains(PTY_MARKER) {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(seen, "the shell's output came back through the bridge");
    let _ = socket.close(None).await;
}

// ---- scenarios ----

async fn golden_path(binding: &str) {
    let proxy = Proxy::start(None).await;
    let capture = Capture::start(proxy.addr).await;
    let relay_url = format!("http://127.0.0.1:{}", capture.addr.port());

    let root_a = fixture_dir("root-a");
    let root_b = fixture_dir("root-b");
    let outside = fixture_dir("outside");
    let in_b = root_b.join("notes.txt");
    let out = outside.join("secret.txt");
    std::fs::write(&in_b, ROOT_B_MARKER).unwrap();
    std::fs::write(&out, OUTSIDE_MARKER).unwrap();

    let mut env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .scripts(vec![
            tool_step("read", json!({ "path": in_b.display().to_string() })),
            tool_step("read", json!({ "path": out.display().to_string() })),
            text_step(FINAL_MARKER),
        ])
        .serve_arg("--relay")
        .serve_arg(relay_url.clone())
        .serve_arg("--relay-transport")
        .serve_arg(binding)
        .build()
        .await
        .expect("e2e env");
    let local = env.backend.url.clone();
    let link = link_of(&env);
    assert!(
        link.starts_with(&format!(
            "hya+insecure://127.0.0.1:{}/",
            capture.addr.port()
        )),
        "the link names the relay's public URL (the capture hop)"
    );
    assert!(link.contains(&format!("t={binding}")), "pinned binding");
    wait_relay_connected(&env).await;
    let status = relay_cli(&env, &["status", "--json"]);
    assert_eq!(status["binding"], json!(binding), "{status}");

    let bridge = Bridge::start(&link, None).await;
    route_through(&mut env, &bridge.url);
    let api = bridge.url.clone();

    // Bootstrap and health.
    let health = env
        .get_json("/v1/health")
        .await
        .expect("health over the bridge");
    assert_eq!(health["ok"], json!(true), "{health}");
    let bootstrap = env
        .get_json("/v1/bootstrap")
        .await
        .expect("bootstrap over the bridge");
    assert!(bootstrap.is_object(), "{bootstrap}");

    // A two-root Project created remotely; a session with no workdir lands in roots[0].
    let (status, project) = post(
        &format!("{api}/v1/projects"),
        json!({
            "name": "relay-project",
            "roots": [root_a.display().to_string(), root_b.display().to_string()],
        }),
    )
    .await;
    assert_eq!(status, 200, "{project}");
    let project_id = project["id"].as_str().expect("project id").to_owned();
    let (status, created) = post(
        &format!("{api}/v1/sessions"),
        json!({"agent": env.agent, "model": env.model, "projectId": project_id}),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(
        created["session"]["workdir"],
        json!(root_a.display().to_string()),
        "no workdir → the Project's primary root"
    );
    let session: hya_proto::SessionId = created["session"]["id"]
        .as_str()
        .expect("session id")
        .parse()
        .expect("session id parses");

    // The session SSE stream, through the bridge, before the prompt.
    let sse = Sse::open(&format!("{api}/v1/sessions/{session}/events/stream")).await;

    // The prompt: a read in root #2 (no ask) and one outside every root (asks).
    let watcher = async {
        let id = env
            .wait_permission_id(WAIT)
            .await
            .expect("an ExternalDirectory ask over the bridge");
        let pending = env.list_permissions().await.expect("list permissions");
        let row = pending
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == json!(id)))
            .cloned()
            .expect("the ask row");
        env.reply_permission(&id, "once")
            .await
            .expect("answer the ask over the bridge");
        row
    };
    let (turn, ask) = tokio::join!(
        env.prompt(session, format!("read both files {PROMPT_MARKER}")),
        watcher
    );
    turn.expect("the turn completes over the bridge");
    assert_eq!(
        ask["payload"]["action"],
        json!("externaldirectory"),
        "{ask}"
    );
    assert_eq!(
        ask["payload"]["resource"],
        json!(format!("{}/*", outside.display())),
        "only the outside read asks; root #2 is inside the Project"
    );
    let asks = env.list_permissions().await.expect("list permissions");
    assert!(asks.as_array().is_none_or(Vec::is_empty), "{asks}");

    // Both tool results reached the model and the transcript.
    let requests = env.fake_requests().expect("fake requests");
    let later = fake_requests_from(&requests, 1);
    assert!(later.contains(ROOT_B_MARKER), "root #2 read result");
    assert!(later.contains(OUTSIDE_MARKER), "outside read result");
    assert!(fake_requests_from(&requests, 0).contains(PROMPT_MARKER));
    let messages = env
        .get_json(&format!("/v1/sessions/{session}/messages"))
        .await
        .expect("messages over the bridge")
        .to_string();
    for marker in [ROOT_B_MARKER, OUTSIDE_MARKER, FINAL_MARKER, PROMPT_MARKER] {
        assert!(messages.contains(marker), "transcript lacks {marker}");
    }

    // The SSE stream saw the ask and the turn finish.
    sse.wait_for("the ask and the final assistant message", |frames| {
        let text = |frame: &Value| frame.to_string();
        let asked = frames.iter().any(|f| text(f).contains("externaldirectory"));
        let last_text = frames.iter().rposition(|f| text(f).contains(FINAL_MARKER));
        let finished = last_text.is_some_and(|at| {
            frames[at..]
                .iter()
                .any(|f| !f["event"]["messageFinished"].is_null())
        });
        asked && finished
    })
    .await;
    drop(sse);

    // A PTY over the bridge.
    pty_echo(&api, &root_a).await;

    // A temporary session over the bridge.
    let (status, temporary) = post(
        &format!("{api}/v1/sessions"),
        json!({"agent": env.agent, "model": env.model, "kind": "SESSION_KIND_TEMPORARY"}),
    )
    .await;
    assert_eq!(status, 200, "{temporary}");
    assert_eq!(
        temporary["session"]["kind"],
        json!("SESSION_KIND_TEMPORARY")
    );
    assert!(
        temporary["session"]["projectId"]
            .as_str()
            .is_none_or(str::is_empty)
    );

    // RelayControl is refused from relay origin, allowed on loopback.
    let (status, refused) = fresh_get(&format!("{api}/v1/relay/link")).await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["error"]["code"], json!("permission_denied"));
    assert!(!refused.to_string().contains('#'));
    let (status, own) = fresh_get(&format!("{local}/v1/relay/link")).await;
    assert_eq!(status, 200, "{own}");
    assert_eq!(own["link"], json!(link));

    // Rotation: the running bridge is rejected, a new bridge with the new link works.
    // The proxy refuses the old link's open token like an offline room (it
    // never reaches the backend), so the bridge cannot tell it from offline.
    let rotated = relay_cli(&env, &["rotate", "--json"]);
    let new_link = rotated["link"].as_str().expect("rotated link").to_owned();
    assert_ne!(new_link, link);
    let (_, rejected) = wait_get(
        &format!("{api}/v1/health"),
        "the old bridge is rejected after rotate",
        |status, body| {
            status == 503
                && body["error"]["code"] == "unavailable"
                && body["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("offline") || m.contains("rejected the relay link"))
        },
    )
    .await;
    assert!(!rejected.to_string().contains('#'), "{rejected}");
    drop(bridge);
    wait_relay_connected(&env).await;
    let bridge = Bridge::start(&new_link, None).await;
    route_through(&mut env, &bridge.url);
    let api = bridge.url.clone();
    let health = env.get_json("/v1/health").await.expect("health, new link");
    assert_eq!(health["ok"], json!(true));

    // Shutdown: a bridge-side SSE client gets serverStopping, then the bridge
    // answers 503 "remote backend is offline".
    let global = Sse::open(&format!("{api}/v1/events/stream")).await;
    // SAFETY: `kill` has no memory-safety preconditions; the pid is our child.
    unsafe {
        libc::kill(env.backend.pid() as libc::pid_t, libc::SIGTERM);
    }
    let frames = global
        .wait_for("serverStopping", |frames| {
            frames
                .iter()
                .any(|f| !f["event"]["serverStopping"].is_null())
        })
        .await;
    let stopping = frames
        .iter()
        .find(|f| !f["event"]["serverStopping"].is_null())
        .unwrap();
    assert_eq!(
        stopping["event"]["serverStopping"]["reason"],
        json!("signal"),
        "SIGTERM"
    );
    wait_get(
        &format!("{api}/v1/health"),
        "the bridge reports the backend offline",
        |status, body| {
            status == 503
                && body["error"]["code"] == "unavailable"
                && body["error"]["message"] == "remote backend is offline"
        },
    )
    .await;
    drop(global);

    // The proxy saw ciphertext only, for everything above.
    let mut secrets: Vec<(String, Vec<u8>)> = [
        ("the prompt", PROMPT_MARKER),
        ("root #2's file contents", ROOT_B_MARKER),
        ("the outside file's contents", OUTSIDE_MARKER),
        ("the assistant's answer", FINAL_MARKER),
        ("the PTY output", PTY_MARKER),
    ]
    .into_iter()
    .map(|(name, marker)| (name.to_owned(), marker.as_bytes().to_vec()))
    .collect();
    secrets.extend(link_secrets(&link));
    secrets.extend(
        link_secrets(&new_link)
            .into_iter()
            .map(|(name, needle)| (format!("{name} after rotate"), needle)),
    );
    capture.assert_blind(Some(&bridge.room), &secrets);

    drop(bridge);
    drop(env);
    for dir in [root_a, root_b, outside] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t2_35_relay_golden_path_over_grpc() {
    golden_path("grpc").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t2_36_relay_golden_path_over_websocket() {
    golden_path("ws").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t2_37_relay_over_tls_with_a_private_ca() {
    let tls = Tls::new();
    let proxy = Proxy::start(Some(&tls)).await;
    let capture = Capture::start(proxy.addr).await;
    let relay_url = format!("https://localhost:{}", capture.addr.port());

    let mut env = E2eEnvBuilder::new()
        .scripts(vec![text_step(FINAL_MARKER)])
        .serve_arg("--relay")
        .serve_arg(relay_url)
        .serve_arg("--relay-ca")
        .serve_arg(tls.cert.display().to_string())
        .build()
        .await
        .expect("e2e env");
    let link = link_of(&env);
    assert!(
        link.starts_with(&format!("hya://localhost:{}/", capture.addr.port())),
        "TLS relay → hya:// link"
    );
    wait_relay_connected(&env).await;
    let bridge = Bridge::start(&link, Some(&tls.cert)).await;
    route_through(&mut env, &bridge.url);

    let health = env.get_json("/v1/health").await.expect("health over TLS");
    assert_eq!(health["ok"], json!(true));
    let session = env.create_session().await.expect("session over TLS");
    env.prompt(session, format!("hello over tls {PROMPT_MARKER}"))
        .await
        .expect("prompt over TLS");
    let messages = env
        .get_json(&format!("/v1/sessions/{session}/messages"))
        .await
        .expect("messages over TLS")
        .to_string();
    assert!(messages.contains(FINAL_MARKER) && messages.contains(PROMPT_MARKER));

    let mut secrets = vec![
        ("the prompt".to_owned(), PROMPT_MARKER.as_bytes().to_vec()),
        ("the answer".to_owned(), FINAL_MARKER.as_bytes().to_vec()),
    ];
    secrets.extend(link_secrets(&link));
    // TLS hides even the room id from this hop, so there is no plaintext control.
    capture.assert_blind(None, &secrets);
    drop(bridge);
}
