//! The persistent backend daemon (ADR-0023; docs/cli.md "`hya serve`
//! daemon control"): `hya serve start` runs `hya serve` detached (its own
//! session, output to `<db>.server.log`) and returns once it answers; the
//! daemon outlives the starter and every client. `status` reports it from the
//! discovery file plus a health probe, `stop` ends it gracefully (even with
//! clients still streaming), and `restart` hands a successor generation the
//! listener, the lock, and the open turns through the recorded journal: the
//! default response acknowledges the queued handoff, and the successor keeps
//! the old URL. Concurrent starters end up on one daemon (the database lock
//! arbitrates).

use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.canonicalize()?)
}

/// `hya serve <args> --db <db>` with an isolated HOME/XDG under `root`.
fn serve(root: &Path, db: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .current_dir(root)
        .arg("serve")
        .args(args)
        .arg("--db")
        .arg(db)
        .stdin(Stdio::null());
    command
}

fn run(root: &Path, db: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(serve(root, db, args).output()?)
}

fn json(output: &Output) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).map_err(|error| {
        format!(
            "not JSON ({error}): {text}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into()
    })
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn pid_of(value: &serde_json::Value) -> Result<i32, Box<dyn std::error::Error>> {
    Ok(i32::try_from(value["pid"].as_i64().ok_or("no pid")?)?)
}

/// Kills every daemon a test started, even when it fails half-way.
struct Daemons(Vec<i32>);

impl Drop for Daemons {
    fn drop(&mut self) {
        for pid in &self.0 {
            // SAFETY: `kill` has no memory-safety preconditions.
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
    }
}

/// `host:port` of `http://host:port`.
fn authority(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(url
        .strip_prefix("http://")
        .ok_or("not http")?
        .trim_end_matches('/')
        .to_string())
}

/// `GET <url><path>` over a raw connection; the status line and body.
fn http_get(url: &str, path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut socket = TcpStream::connect(authority(url)?)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        socket,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut text = String::new();
    socket.read_to_string(&mut text)?;
    Ok(text)
}

/// Open `GET /v1/events/stream` (SSE) and wait for its response head.
fn open_events(url: &str) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(authority(url)?)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    write!(
        stream,
        "GET /v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n"
    )?;
    let mut head = [0u8; 64];
    let read = stream.read(&mut head)?;
    assert!(String::from_utf8_lossy(&head[..read]).starts_with("HTTP/1.1 200"));
    Ok(stream)
}

/// Everything the server still sends on `stream` until it closes it.
fn rest_of(mut stream: TcpStream) -> String {
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn wait_until(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) -> TestResult {
    let deadline = Instant::now() + timeout;
    while !check() {
        if Instant::now() > deadline {
            return Err(format!("timed out waiting until {what}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Whether `GET <url>/v1/health` answers ok.
fn healthy(url: &str) -> bool {
    http_get(url, "/v1/health")
        .map(|text| text.contains("\"ok\":true"))
        .unwrap_or(false)
}

/// Poll discovery and health until a successor generation serves `url`: a
/// `status --json` pid other than `old_pid`, same URL, `/v1/health` ok.
/// This is how a caller observes a handoff whose `restart` command returned
/// at the `queued` acknowledgement.
fn wait_for_successor(
    root: &Path,
    db: &Path,
    old_pid: i32,
    url: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Ok(output) = run(root, db, &["status", "--json"])
            && output.status.success()
        {
            let status = json(&output)?;
            if pid_of(&status)? != old_pid && status["url"].as_str() == Some(url) && healthy(url) {
                return Ok(status);
            }
        }
        if Instant::now() > deadline {
            return Err(
                format!("timed out waiting for a successor of pid {old_pid} at {url}").into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The handoff journal's last stage name, when it exists.
fn journal_stage(path: &Path) -> Option<String> {
    let journal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    journal["stages"].as_array()?.last()?["stage"]
        .as_str()
        .map(str::to_owned)
}

/// One JSON HTTP request to the served backend; `(status, body)`. A `null`
/// body sends no payload. Chunked bodies are decoded.
fn http_json(
    url: &str,
    method: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<(u16, serde_json::Value), Box<dyn std::error::Error>> {
    let mut socket = TcpStream::connect(authority(url)?)?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nconnection: close\r\n");
    if body.is_null() {
        request.push_str("\r\n");
    } else {
        let payload = body.to_string();
        request.push_str(&format!(
            "content-type: application/json\r\ncontent-length: {}\r\n\r\n{payload}",
            payload.len()
        ));
    }
    socket.write_all(request.as_bytes())?;
    socket.flush()?;
    let mut raw = String::new();
    socket.read_to_string(&mut raw)?;
    let (head, text) = raw.split_once("\r\n\r\n").ok_or("no HTTP body")?;
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("no HTTP status")?;
    let text = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(text)
    } else {
        text.to_string()
    };
    let parsed: serde_json::Value =
        serde_json::from_str(text.trim()).map_err(|error| format!("not JSON ({error}): {text}"))?;
    Ok((status, parsed))
}

/// Concatenate the chunks of a chunked-transfer body.
fn dechunk(body: &str) -> String {
    let mut text = String::new();
    let mut rest = body;
    while let Some((size_line, remainder)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("0").trim(), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        let (Some(chunk_text), Some(after)) = (remainder.get(..size), remainder.get(size..)) else {
            break;
        };
        text.push_str(chunk_text);
        rest = after.strip_prefix("\r\n").unwrap_or("");
    }
    text
}

/// Create a session on the served backend; its id.
fn create_session(url: &str, workdir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let (status, created) = http_json(
        url,
        "POST",
        "/v1/sessions",
        &serde_json::json!({
            "agent": "hya-main",
            "model": "fake/model",
            "workdir": workdir.display().to_string(),
        }),
    )?;
    if !(200..300).contains(&status) {
        return Err(format!("create session failed ({status}): {created}").into());
    }
    Ok(created["session"]["id"]
        .as_str()
        .ok_or(format!("no session id: {created}"))?
        .to_string())
}

/// Admit one v1 turn: a user prompt.
fn prompt(url: &str, session: &str, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (status, admitted) = http_json(
        url,
        "POST",
        &format!("/v1/sessions/{session}/turns"),
        &serde_json::json!({ "prompt": { "text": text } }),
    )?;
    if !(200..300).contains(&status) {
        return Err(format!("prompt failed ({status}): {admitted}").into());
    }
    Ok(())
}

/// The session transcript, flattened to text for matching.
fn transcript(url: &str, session: &str) -> Result<String, Box<dyn std::error::Error>> {
    let (status, messages) = http_json(
        url,
        "GET",
        &format!("/v1/sessions/{session}/messages"),
        &serde_json::Value::Null,
    )?;
    if !(200..300).contains(&status) {
        return Err(format!("transcript failed ({status}): {messages}").into());
    }
    Ok(messages.to_string())
}

/// Whether `session` is listed without a busy flag.
fn session_idle(url: &str, session: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let (status, listed) = http_json(url, "GET", "/v1/sessions", &serde_json::Value::Null)?;
    if !(200..300).contains(&status) {
        return Err(format!("session list failed ({status}): {listed}").into());
    }
    Ok(listed["sessions"].as_array().is_some_and(|sessions| {
        sessions
            .iter()
            .find(|entry| entry["id"].as_str() == Some(session))
            .is_some_and(|entry| entry["busy"] != serde_json::json!(true))
    }))
}

/// Point the daemon at `base_url` with an isolated config (the shape
/// `hya-e2e` writes): one OpenAI-compatible provider, permissive permissions.
fn write_provider_config(root: &Path, base_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = root.join("config").join("hya");
    std::fs::create_dir_all(config.join("auth"))?;
    std::fs::write(
        config.join("config.yaml"),
        format!(
            "default_model: fake/model
providers:
  fake:
    kind: openai-compatible
    base_url: {base_url}
    api_key: e2e-test-key
    models:
      - id: model
plugins: {{}}
permission:
  model: allow
  rules: []
"
        ),
    )?;
    std::fs::write(
        config.join("auth").join("fake.yaml"),
        "token: e2e-test-key\n",
    )?;
    Ok(())
}

/// Single-quoted shell word for a path.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display())
}

/// One scripted provider round: assistant text, or tool calls to run.
enum FakeStep {
    Text(String),
    ToolCalls(Vec<(&'static str, serde_json::Value)>),
}

/// Opening of the fixed `title` agent's system prompt: background title
/// calls are answered off the script so a turn's rounds stay deterministic.
const TITLE_AGENT_MARKER: &str = "You are a title generator.";

/// A scripted OpenAI-compatible provider serving `/v1/chat/completions` as
/// SSE (the wire shapes `hya-e2e`'s FakeLlm serves): every model round pops
/// the next step from the script, so a turn's shell rounds and its final
/// text are pinned to the generations that resume them.
struct FakeProvider {
    base_url: String,
    _listener: TcpListener,
    stop: Arc<AtomicBool>,
    steps: Arc<Mutex<VecDeque<FakeStep>>>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl FakeProvider {
    fn start(steps: Vec<FakeStep>) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let provider = Self {
            base_url: format!("http://{}/v1", listener.local_addr()?),
            stop: Arc::new(AtomicBool::new(false)),
            steps: Arc::new(Mutex::new(steps.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
            _listener: listener.try_clone()?,
        };
        let acceptor = listener;
        let stop = Arc::clone(&provider.stop);
        let steps = Arc::clone(&provider.steps);
        let requests = Arc::clone(&provider.requests);
        std::thread::spawn(move || {
            let _ = acceptor.set_nonblocking(true);
            while !stop.load(Ordering::Relaxed) {
                match acceptor.accept() {
                    Ok((stream, _)) => {
                        let steps = Arc::clone(&steps);
                        let requests = Arc::clone(&requests);
                        std::thread::spawn(move || {
                            let _ = serve_provider_connection(stream, &steps, &requests);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(provider)
    }

    /// OpenAI-compatible base URL, `/v1` included, for the backend config.
    fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Scripted steps no model round consumed.
    fn steps_left(&self) -> usize {
        self.steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Recorded chat request bodies, in arrival order.
    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Answer one provider connection: exactly one request, `connection: close`.
fn serve_provider_connection(
    mut stream: TcpStream,
    steps: &Arc<Mutex<VecDeque<FakeStep>>>,
    requests: &Arc<Mutex<Vec<serde_json::Value>>>,
) -> Option<()> {
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let body_start = head_end + 4;
    while raw.len() < body_start + length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
    }
    if raw.len() < body_start {
        return None;
    }
    let body = String::from_utf8_lossy(&raw[body_start..]).into_owned();
    let mut parts = head.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let (status, content_type, payload) = provider_response(method, path, &body, steps, requests)?;
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes()).ok()?;
    stream.flush().ok()?;
    Some(())
}

/// The scripted response for one provider request.
fn provider_response(
    method: &str,
    path: &str,
    body: &str,
    steps: &Arc<Mutex<VecDeque<FakeStep>>>,
    requests: &Arc<Mutex<Vec<serde_json::Value>>>,
) -> Option<(&'static str, &'static str, String)> {
    if method == "GET" && path == "/v1/models" {
        return Some((
            "200 OK",
            "application/json",
            r#"{"object":"list","data":[{"id":"model"}]}"#.to_string(),
        ));
    }
    if method != "POST" || !path.ends_with("/chat/completions") {
        return Some(("404 Not Found", "text/plain", "not scripted\n".to_string()));
    }
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    if system_text(&parsed).contains(TITLE_AGENT_MARKER) {
        return Some(("200 OK", "text/event-stream", sse(&text_frames(""))));
    }
    let step = steps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop_front();
    requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(parsed);
    let frames = match step {
        Some(FakeStep::Text(text)) => text_frames(&text),
        Some(FakeStep::ToolCalls(calls)) => tool_frames(&calls),
        // The script drove every round; an extra round ends the turn.
        None => text_frames(""),
    };
    Some(("200 OK", "text/event-stream", sse(&frames)))
}

/// SSE frames streaming `text`, then stopping.
fn text_frames(text: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        serde_json::json!({"choices":[{"delta":{"content": text},"finish_reason":null}]}),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        serde_json::Value::String("[DONE]".into()),
    ]
}

/// SSE frames streaming one tool call per pair, then finishing tool_calls.
fn tool_frames(calls: &[(&'static str, serde_json::Value)]) -> Vec<serde_json::Value> {
    let mut frames = Vec::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        frames.push(serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": index,
                    "id": format!("call_{index}"),
                    "type": "function",
                    "function": {"name": name, "arguments": ""}
                }]},
                "finish_reason": null
            }]
        }));
        frames.push(serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": index,
                    "function": {"arguments": arguments.to_string()}
                }]},
                "finish_reason": null
            }]
        }));
    }
    frames.push(serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}));
    frames.push(serde_json::Value::String("[DONE]".into()));
    frames
}

/// Render frames as an SSE body.
fn sse(frames: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for frame in frames {
        match frame {
            serde_json::Value::String(done) => body.push_str(&format!("data: {done}\n\n")),
            other => body.push_str(&format!("data: {other}\n\n")),
        }
    }
    body
}

/// The concatenated `system`-role contents of a chat body.
fn system_text(body: &serde_json::Value) -> String {
    body["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .filter(|message| message["role"] == serde_json::json!("system"))
                .filter_map(|message| message["content"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[test]
fn start_runs_a_detached_daemon_that_status_reports_and_stop_ends() -> TestResult {
    let root = scratch("hya-daemon")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());

    let started = run(&root, &db, &["start", "--json"])?;
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let info = json(&started)?;
    let pid = pid_of(&info)?;
    daemons.0.push(pid);
    assert_eq!(info["started"], serde_json::json!(true));
    assert_eq!(
        info["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(info["db"].as_str(), Some(db.to_string_lossy().as_ref()));
    let url = info["url"].as_str().ok_or("no url")?.to_string();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let log = PathBuf::from(format!("{}.server.log", db.display()));
    assert_eq!(info["log"].as_str(), Some(log.to_string_lossy().as_ref()));

    // The starter has exited; the daemon runs on, in its own session.
    assert!(alive(pid));
    // SAFETY: getsid only reads process attributes.
    let (ours, theirs) = unsafe { (libc::getsid(0), libc::getsid(pid)) };
    assert_ne!(
        ours, theirs,
        "the daemon must not share the starter's session"
    );
    assert!(http_get(&url, "/v1/health")?.contains("\"ok\":true"));
    assert!(log.is_file(), "the daemon logs to {}", log.display());

    // A second start finds it.
    let again = json(&run(&root, &db, &["start", "--json"])?)?;
    assert_eq!(again["started"], serde_json::json!(false));
    assert_eq!(pid_of(&again)?, pid);
    assert_eq!(again["url"].as_str(), Some(url.as_str()));

    // Status: human and JSON.
    let status = run(&root, &db, &["status"])?;
    assert!(status.status.success());
    let text = String::from_utf8_lossy(&status.stdout);
    for needle in [
        url.as_str(),
        &format!("pid {pid}"),
        env!("CARGO_PKG_VERSION"),
        "uptime",
        &db.to_string_lossy(),
    ] {
        assert!(text.contains(needle), "status lacks {needle}: {text}");
    }
    let status = json(&run(&root, &db, &["status", "--json"])?)?;
    assert_eq!(pid_of(&status)?, pid);
    assert!(status["uptimeMs"].as_u64().is_some(), "{status}");

    // Stop: graceful, even while a client holds an event stream open.
    let stream = open_events(&url)?;
    let begun = Instant::now();
    let stopped = run(&root, &db, &["stop"])?;
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(
        begun.elapsed() < Duration::from_secs(15),
        "an open client stream must not hold the shutdown open ({:?})",
        begun.elapsed()
    );
    assert!(String::from_utf8_lossy(&stopped.stdout).contains(&format!("pid {pid}")));
    // The client was told it was a manual stop (so it does not start the
    // next daemon), and the output says so.
    let told = rest_of(stream);
    assert!(
        told.contains(r#""serverStopping":{"reason":"stop"}"#),
        "the stream's last frame names the stop: {told}"
    );
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("stay disconnected until /reconnect"),
        "{}",
        String::from_utf8_lossy(&stopped.stdout)
    );
    assert!(
        !PathBuf::from(format!("{}.server.stop", db.display())).exists(),
        "the daemon consumed the stop request"
    );
    wait_until("the daemon exited", Duration::from_secs(5), || !alive(pid))?;
    assert!(!PathBuf::from(format!("{}.server.json", db.display())).exists());

    let status = run(&root, &db, &["status"])?;
    assert!(!status.status.success(), "status of a stopped server fails");
    assert!(String::from_utf8_lossy(&status.stderr).contains("no hya server is running"));
    // Stopping nothing is not an error.
    let idle = run(&root, &db, &["stop"])?;
    assert!(idle.status.success());
    assert!(String::from_utf8_lossy(&idle.stdout).contains("no hya server is running"));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn restart_replaces_the_daemon() -> TestResult {
    let root = scratch("hya-daemon-restart")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let url = first["url"].as_str().ok_or("no url")?.to_string();
    let stream = open_events(&url)?;

    // The default restart answers as soon as the old generation queued the
    // handoff; the successor takes over while the caller has moved on.
    let restarted = run(&root, &db, &["restart", "--json"])?;
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let queued = json(&restarted)?;
    assert_eq!(queued["queued"], serde_json::json!(true), "{queued}");
    assert_eq!(queued["started"], serde_json::json!(false), "{queued}");
    assert_eq!(
        pid_of(&queued)?,
        first_pid,
        "the old generation acknowledged the handoff: {queued}"
    );
    assert_eq!(
        queued["url"].as_str(),
        Some(url.as_str()),
        "restart must retain the listener: {queued}"
    );
    assert_eq!(
        queued["db"].as_str(),
        Some(db.to_string_lossy().as_ref()),
        "{queued}"
    );

    // Clients are told why their stream ends, then reconnect to the same URL.
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
    let told = rest_of(stream);
    assert!(
        told.contains(r#""serverStopping":{"reason":"restart"}"#),
        "clients of a restarted daemon wait for the next one: {told}"
    );

    // Poll the successor in: same URL, new pid, healthy, old pid gone.
    let second = wait_for_successor(&root, &db, first_pid, &url)?;
    let second_pid = pid_of(&second)?;
    daemons.0.push(second_pid);
    assert!(
        http_get(&url, "/v1/health")?.contains("\"ok\":true"),
        "the successor answers health at {url}"
    );
    wait_until("the old generation exited", Duration::from_secs(10), || {
        !alive(first_pid)
    })?;
    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || {
        !alive(second_pid)
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// `hya serve restart` must hand off to a successor through the recorded
/// journal stages (`requested -> queued -> released -> ready -> transferred`)
/// instead of releasing the listener. The default response acknowledges the
/// queued handoff (`pid` is the old generation; `requested` names the restart
/// CLI), and the test then polls the successor in: same URL and status
/// timestamp, new pid, health, with the journal naming who wrote each stage.
#[test]
fn restart_hands_off_through_the_recorded_journal_stages() -> TestResult {
    let root = scratch("hya-daemon-journal")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let url = first["url"].as_str().ok_or("no url")?.to_string();

    let journal_path = PathBuf::from(format!("{}.server.handoff", db.display()));
    let restarted = run(&root, &db, &["restart", "--json"])?;
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let queued = json(&restarted)?;
    assert_eq!(queued["queued"], serde_json::json!(true), "{queued}");
    assert_eq!(
        pid_of(&queued)?,
        first_pid,
        "the old generation queued the handoff: {queued}"
    );
    assert_eq!(
        queued["url"].as_str(),
        Some(url.as_str()),
        "handoff keeps the listener: {queued}"
    );

    let second = wait_for_successor(&root, &db, first_pid, &url)?;
    let second_pid = pid_of(&second)?;
    daemons.0.push(second_pid);
    wait_until("the old generation exited", Duration::from_secs(10), || {
        !alive(first_pid)
    })?;
    wait_until("the journal transfer", Duration::from_secs(15), || {
        journal_stage(&journal_path).as_deref() == Some("transferred")
    })?;

    let text = std::fs::read_to_string(&journal_path).map_err(|error| {
        format!(
            "restart must record the handoff journal at {}: {error}",
            journal_path.display()
        )
    })?;
    let journal: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(journal["mode"], serde_json::json!("handoff"), "{text}");
    let stages = journal["stages"].as_array().ok_or("no journal stages")?;
    let names: Vec<&str> = stages
        .iter()
        .map(|stage| stage["stage"].as_str().ok_or("no stage name"))
        .collect::<Result<_, _>>()?;
    assert_eq!(
        names,
        ["requested", "queued", "released", "ready", "transferred"],
        "journal: {text}"
    );
    // The old generation queued and released; the successor readied and
    // completed the transfer. `requested` names the restart CLI.
    let stage_pid =
        |index: usize| -> Result<i32, Box<dyn std::error::Error>> { pid_of(&stages[index]) };
    assert_ne!(
        stage_pid(0)?,
        first_pid,
        "requested names the restart CLI: {text}"
    );
    assert_ne!(stage_pid(0)?, second_pid, "{text}");
    assert_eq!(stage_pid(1)?, first_pid, "{text}");
    assert_eq!(stage_pid(2)?, first_pid, "{text}");
    assert_eq!(stage_pid(3)?, second_pid, "{text}");
    assert_eq!(stage_pid(4)?, second_pid, "{text}");

    // Same listener, new generation, inherited status.
    assert_eq!(second["url"].as_str(), Some(url.as_str()), "{second}");
    assert_ne!(first_pid, second_pid);
    assert_eq!(
        second["startedAt"], first["startedAt"],
        "the successor inherits the old generation's status timestamp"
    );
    assert!(
        http_get(&url, "/v1/health")?.contains("\"ok\":true"),
        "the successor answers health at {url}"
    );

    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || {
        !alive(second_pid)
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The shell-invoked restart — a model's bash tool running `hya serve
/// restart` on its own daemon — works only because the default response
/// returns at `queued`: the tool result commits while its own generation
/// still serves the turn, that generation checkpoints the turn at the round
/// boundary, and the successor continues it without a new prompt. Two
/// consecutive restarts must each hand off cleanly, and each shell command
/// must run exactly once: the successor resumes from the committed result
/// instead of replaying the call.
#[test]
fn restart_from_a_shell_turn_continues_in_the_successor() -> TestResult {
    let root = scratch("hya-daemon-shell-restart")?;
    let db = root.join("s.db");
    std::fs::create_dir_all(root.join("project"))?;
    let mut daemons = Daemons(Vec::new());

    // Two scripted rounds call the bash tool (each writes one side effect and
    // restarts the daemon through the built binary); the third only runs in
    // the final successor.
    let effects = root.join("side-effects.log");
    let restart_command = format!(
        "printf 'restarted\\n' >> {} && {} serve restart --db {}",
        quoted(&effects),
        quoted(Path::new(env!("CARGO_BIN_EXE_hya"))),
        quoted(&db),
    );
    let tool_args = serde_json::json!({ "command": restart_command });
    let tool_round = || FakeStep::ToolCalls(vec![("bash", tool_args.clone())]);
    let provider = FakeProvider::start(vec![
        tool_round(),
        tool_round(),
        FakeStep::Text("RESTART_CONTINUATION_OK".to_string()),
    ])?;
    write_provider_config(&root, provider.base_url())?;

    let first = json(&run(&root, &db, &["start", "--json", "--yolo"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let url = first["url"].as_str().ok_or("no url")?.to_string();

    let session = create_session(&url, &root.join("project"))?;
    prompt(
        &url,
        &session,
        "restart the backend with bash twice, then report",
    )?;

    let journal_path = PathBuf::from(format!("{}.server.handoff", db.display()));
    if wait_until(
        "the successor continued the turn",
        Duration::from_secs(120),
        || {
            transcript(&url, &session)
                .map(|text| text.contains("RESTART_CONTINUATION_OK"))
                .unwrap_or(false)
        },
    )
    .is_err()
    {
        return Err(format!(
            "the turn never continued: journal at {:?}, {} scripted steps left, transcript: {}",
            journal_stage(&journal_path),
            provider.steps_left(),
            transcript(&url, &session)?
        )
        .into());
    }
    wait_until("the session went idle", Duration::from_secs(30), || {
        session_idle(&url, &session).unwrap_or(false)
    })?;

    // One session carried the whole turn across both handoffs: the prompt,
    // both shell rounds, and the continuation are one transcript.
    let transcript = transcript(&url, &session)?;
    assert!(
        transcript.contains("restart the backend with bash twice"),
        "{transcript}"
    );
    assert!(transcript.contains("serve restart"), "{transcript}");

    // Each shell command ran exactly once — two restarts, two side effects,
    // none replayed by a successor.
    let effects_text = std::fs::read_to_string(&effects)?;
    assert_eq!(
        effects_text.lines().count(),
        2,
        "no shell command may run twice: {effects_text}"
    );
    assert_eq!(provider.steps_left(), 0, "the script drove every round");
    assert_eq!(
        provider.requests().len(),
        3,
        "three model rounds: tool, continued tool, final text"
    );

    // The final journal names the second handoff: queued and released by the
    // middle generation, readied and transferred by the last one.
    wait_until(
        "the final journal transfer",
        Duration::from_secs(15),
        || journal_stage(&journal_path).as_deref() == Some("transferred"),
    )?;
    let journal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&journal_path)?)?;
    let stages = journal["stages"].as_array().ok_or("no journal stages")?;
    let stage_pid =
        |index: usize| -> Result<i32, Box<dyn std::error::Error>> { pid_of(&stages[index]) };
    let middle_pid = stage_pid(1)?;
    daemons.0.push(middle_pid);
    assert_ne!(
        first_pid, middle_pid,
        "the first handoff must create a distinct middle generation: {journal}"
    );
    assert_ne!(
        middle_pid,
        stage_pid(3)?,
        "the middle generation must differ from the final successor: {journal}"
    );

    // Same URL, new generation, healthy.
    let status = wait_for_successor(&root, &db, middle_pid, &url)?;
    let final_pid = pid_of(&status)?;
    daemons.0.push(final_pid);
    assert_eq!(final_pid, stage_pid(4)?, "{status}");

    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || {
        !alive(final_pid)
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn concurrent_starts_share_one_daemon() -> TestResult {
    let root = scratch("hya-daemon-race")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let starters: Vec<_> = (0..3)
        .map(|_| {
            serve(&root, &db, &["start", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .collect::<Result<_, _>>()?;
    let mut infos = Vec::new();
    for starter in starters {
        let output = starter.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        infos.push(json(&output)?);
    }
    let pid = pid_of(&infos[0])?;
    daemons.0.push(pid);
    for info in &infos {
        assert_eq!(
            pid_of(info)?,
            pid,
            "every starter reports the one daemon: {infos:?}"
        );
    }
    let started = infos
        .iter()
        .filter(|info| info["started"] == serde_json::json!(true))
        .count();
    assert_eq!(started, 1, "exactly one starter started it: {infos:?}");
    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || !alive(pid))?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn start_waits_out_a_server_that_is_shutting_down() -> TestResult {
    // `restart`-like sequence from two sides: a stop in flight, then a start
    // that finds the lock still held by the stopping server. The start must
    // not fail; it starts the next daemon once the lock is free.
    let root = scratch("hya-daemon-handover")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let stream = open_events(first["url"].as_str().ok_or("no url")?)?;
    // SAFETY: `kill` has no memory-safety preconditions.
    unsafe {
        libc::kill(first_pid, libc::SIGTERM);
    }
    // A plain signal (no `hya serve stop` request) is reported as such.
    let told = rest_of(stream);
    assert!(
        told.contains(r#""serverStopping":{"reason":"signal"}"#),
        "{told}"
    );
    let next = run(&root, &db, &["start", "--json"])?;
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let next = json(&next)?;
    let next_pid = pid_of(&next)?;
    daemons.0.push(next_pid);
    assert_ne!(next_pid, first_pid);
    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || {
        !alive(next_pid)
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// `hya serve check --db` composes the whole runtime against a snapshot of
/// the database, next to a running daemon, without touching its lock,
/// discovery, or runtime-owner claim.
#[test]
fn check_composes_the_runtime_beside_a_running_daemon() -> TestResult {
    let root = scratch("hya-daemon-check")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let discovery = std::fs::read(root.join("s.db.server.json"))?;

    let checked = run(&root, &db, &["check", "--json"])?;
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let report = json(&checked)?;
    assert_eq!(report["ok"], serde_json::json!(true), "{report}");
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"), "{report}");

    let status = json(&run(&root, &db, &["status", "--json"])?)?;
    assert_eq!(pid_of(&status)?, first_pid, "the daemon kept serving");
    assert_eq!(std::fs::read(root.join("s.db.server.json"))?, discovery);
    let leftovers: Vec<_> = std::fs::read_dir(&root)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("check") || name.contains("snapshot"))
        .collect();
    assert!(leftovers.is_empty(), "snapshot left behind: {leftovers:?}");

    // A fresh database is checked in place of nothing: no lock, no file.
    let fresh = root.join("fresh.db");
    let checked = run(&root, &fresh, &["check", "--json"])?;
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(!fresh.exists() && !root.join("fresh.db.lock").exists());

    assert!(run(&root, &db, &["stop"])?.status.success());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A configuration the runtime cannot compose fails the check (the daemon
/// would silently fall back to the offline provider).
#[test]
fn check_fails_on_a_broken_configuration() -> TestResult {
    let root = scratch("hya-daemon-check-bad")?;
    let db = root.join("s.db");
    let config = root.join("config").join("hya");
    std::fs::create_dir_all(&config)?;
    std::fs::write(
        config.join("config.yaml"),
        "providers: [this is not a map\n",
    )?;
    let checked = run(&root, &db, &["check", "--json"])?;
    assert!(!checked.status.success());
    let report = json(&checked)?;
    assert_eq!(report["ok"], serde_json::json!(false), "{report}");
    assert!(
        report["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "{report}"
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A failing `--verify` command refuses the restart before the running
/// daemon is asked for anything: same pid serving, no handoff journal.
#[test]
fn restart_refuses_when_a_verify_command_fails() -> TestResult {
    let root = scratch("hya-daemon-verify")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);

    let refused = run(
        &root,
        &db,
        &["restart", "--verify", "echo proof-output; exit 3"],
    )?;
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("proof-output"), "{stderr}");
    assert!(!root.join("s.db.server.handoff").exists());
    let status = json(&run(&root, &db, &["status", "--json"])?)?;
    assert_eq!(pid_of(&status)?, first_pid);

    let passed = run(&root, &db, &["restart", "--json", "--verify", "true"])?;
    assert!(
        passed.status.success(),
        "{}",
        String::from_utf8_lossy(&passed.stderr)
    );
    let queued = json(&passed)?;
    assert_eq!(queued["check"]["ok"], serde_json::json!(true), "{queued}");
    let url = first["url"].as_str().ok_or("no url")?.to_string();
    let second = wait_for_successor(&root, &db, first_pid, &url)?;
    daemons.0.push(pid_of(&second)?);
    assert!(run(&root, &db, &["stop"])?.status.success());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A successor that passes its self-check but fails to start is replaced by
/// the previous generation's pinned build over the same listener: clients
/// keep one URL, `status` reports the rollback, nothing parks.
#[test]
fn a_successor_that_fails_to_start_rolls_back_to_the_pinned_build() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;
    let root = scratch("hya-daemon-rollback")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let url = first["url"].as_str().ok_or("no url")?.to_string();
    // A "new build" whose self-check delegates to the real hya but whose
    // server refuses to start.
    let bad = root.join("bad-hya");
    std::fs::write(
        &bad,
        format!(
            "#!/bin/sh\ncase \" $* \" in *\" check \"*) exec {} \"$@\";; esac\n\
             echo 'bad build refuses to serve' >&2\nexit 1\n",
            quoted(Path::new(env!("CARGO_BIN_EXE_hya")))
        ),
    )?;
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755))?;

    let restarted = run(
        &root,
        &db,
        &["restart", "--json", "--exe", bad.to_str().ok_or("path")?],
    )?;
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let second = wait_for_successor(&root, &db, first_pid, &url)?;
    let second_pid = pid_of(&second)?;
    daemons.0.push(second_pid);
    let rolled = &second["lastRestart"];
    assert_eq!(rolled["rolledBack"], serde_json::json!(true), "{second}");
    assert!(
        rolled["error"]
            .as_str()
            .is_some_and(|error| error.contains("exit 1")),
        "{second}"
    );
    let command = Command::new("ps")
        .args(["-o", "command=", "-p", &second_pid.to_string()])
        .output()?;
    let command = String::from_utf8_lossy(&command.stdout);
    assert!(
        command.contains("s.db.server.gen"),
        "runs the pinned build: {command}"
    );
    assert!(http_get(&url, "/v1/health")?.contains("\"ok\":true"));
    wait_until("the old generation exited", Duration::from_secs(10), || {
        !alive(first_pid)
    })?;
    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || {
        !alive(second_pid)
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
