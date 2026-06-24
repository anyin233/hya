//! Native in-process transport.
//!
//! Spawns a Bun "native bridge" (`src/cli/tui/native-bridge.ts`) that drives the backend's
//! in-process `app.fetch(...)` directly — no TCP, no HTTP wire protocol. Requests and
//! `GlobalBus` events are exchanged as length-prefixed (4-byte big-endian) JSON frames over the
//! child's stdin/stdout, mirroring the backend's own Worker RPC bridge.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::client::{ApiClient, Transport};
use crate::error::{Result, SdkError};
use crate::types::GlobalEvent;

const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Reject absurd length prefixes so a corrupt/desynced frame fails the bridge instead of
/// allocating gigabytes. Real frames (full session histories) stay well under this.
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

struct FetchOutcome {
    status: u16,
    body: String,
    error: Option<String>,
}

/// Shared bridge state: the framed stdin writer, the in-flight request table, and the id counter.
/// Held by an `Arc` so every [`NativeClient`] and the reader task talk to one channel.
struct BridgeInner {
    stdin: Mutex<tokio::process::ChildStdin>,
    pending: Mutex<HashMap<u64, oneshot::Sender<FetchOutcome>>>,
    next_id: AtomicU64,
}

impl BridgeInner {
    async fn write_frame(&self, value: &Value) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        let len = u32::try_from(body.len())
            .map_err(|_| SdkError::Protocol("frame exceeds 4GiB".to_owned()))?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| SdkError::Bridge(e.to_string()))?;
        stdin
            .write_all(&body)
            .await
            .map_err(|e| SdkError::Bridge(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| SdkError::Bridge(e.to_string()))?;
        Ok(())
    }
}

/// A spawned native bridge process. Owns the child; `Drop` tears down the whole process group so
/// the Bun runtime never leaks.
pub struct NativeBridge {
    child: Child,
    inner: Arc<BridgeInner>,
    reader: tokio::task::JoinHandle<()>,
}

impl NativeBridge {
    /// Spawn the bridge from the backend package directory (the one containing
    /// `src/cli/tui/native-bridge.ts` plus its `node_modules`/`tsconfig.json`) and forward
    /// `GlobalBus` events to `events`. Waits for the bridge's `rpc.ready` handshake.
    ///
    /// # Errors
    /// [`SdkError::Bridge`] if `bun` cannot start or the bridge exits before ready;
    /// [`SdkError::Readiness`] on handshake timeout.
    pub async fn spawn(
        backend_pkg_dir: &Path,
        events: mpsc::UnboundedSender<GlobalEvent>,
    ) -> Result<Self> {
        Self::spawn_with(backend_pkg_dir, events, DEFAULT_READY_TIMEOUT).await
    }

    /// Spawn with an explicit readiness timeout.
    ///
    /// # Errors
    /// See [`NativeBridge::spawn`].
    pub async fn spawn_with(
        backend_pkg_dir: &Path,
        events: mpsc::UnboundedSender<GlobalEvent>,
        ready_timeout: Duration,
    ) -> Result<Self> {
        let script = backend_pkg_dir.join("src/cli/tui/native-bridge.ts");
        // The bridge logs to stderr; capture it to a file so a TUI on the same terminal is never
        // corrupted, while startup failures stay diagnosable.
        let log_path = std::env::temp_dir().join("hya-bridge.log");
        let stderr = std::fs::File::create(&log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let mut child = Command::new("bun")
            .args(["run", "--conditions=browser"])
            .arg(&script)
            .current_dir(backend_pkg_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .process_group(0)
            .spawn()
            .map_err(|e| SdkError::Bridge(format!("spawn bun bridge: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SdkError::Bridge("no stdin pipe".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SdkError::Bridge("no stdout pipe".to_owned()))?;

        let inner = Arc::new(BridgeInner {
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        });

        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let reader = tokio::spawn(read_loop(stdout, Arc::clone(&inner), events, ready_tx));

        match tokio::time::timeout(ready_timeout, ready_rx).await {
            Ok(Ok(())) => Ok(Self {
                child,
                inner,
                reader,
            }),
            Ok(Err(_)) => Err(SdkError::Bridge(format!(
                "bridge exited before ready (see {})",
                log_path.display()
            ))),
            Err(_) => Err(SdkError::Readiness(ready_timeout)),
        }
    }

    /// Build a [`NativeClient`] scoped to `directory` (sent as the directory header). Cheap to
    /// call repeatedly; all clients share this bridge's single channel.
    #[must_use]
    pub fn client(&self, directory: impl Into<String>) -> NativeClient {
        ApiClient::with_transport(NativeTransport {
            inner: Arc::clone(&self.inner),
            directory: directory.into(),
        })
    }
}

impl Drop for NativeBridge {
    fn drop(&mut self) {
        self.reader.abort();
        let Some(pid) = self.child.id() else {
            return;
        };
        // The bridge is its own process-group leader, so signal the negative pid to reach any
        // grandchildren. Graceful SIGTERM, brief wait, then SIGKILL the group.
        let pid = pid as libc::pid_t;
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        for _ in 0..50 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.child.start_kill();
    }
}

async fn read_loop(
    stdout: ChildStdout,
    inner: Arc<BridgeInner>,
    events: mpsc::UnboundedSender<GlobalEvent>,
    ready_tx: oneshot::Sender<()>,
) {
    let mut reader = BufReader::new(stdout);
    let mut ready_tx = Some(ready_tx);
    let mut header = [0u8; 4];
    loop {
        if reader.read_exact(&mut header).await.is_err() {
            break;
        }
        let len = u32::from_be_bytes(header) as usize;
        if len > MAX_FRAME_BYTES {
            break;
        }
        let mut payload = vec![0u8; len];
        if reader.read_exact(&mut payload).await.is_err() {
            break;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&payload) else {
            continue;
        };
        match message.get("type").and_then(Value::as_str) {
            Some("rpc.ready") => {
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(());
                }
            }
            Some("rpc.result") => {
                let Some(id) = message.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                let result = message.get("result");
                let outcome = FetchOutcome {
                    status: result
                        .and_then(|r| r.get("status"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u16,
                    body: result
                        .and_then(|r| r.get("body"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    error: result
                        .and_then(|r| r.get("error"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                };
                if let Some(tx) = inner.pending.lock().await.remove(&id) {
                    let _ = tx.send(outcome);
                }
            }
            Some("rpc.event") => {
                if message.get("event").and_then(Value::as_str) == Some("global.event") {
                    if let Some(data) = message.get("data") {
                        if let Ok(event) = serde_json::from_value::<GlobalEvent>(data.clone()) {
                            if events.send(event).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // Stream closed: fail every in-flight request so callers never hang on a dead bridge.
    let mut pending = inner.pending.lock().await;
    for (_, tx) in pending.drain() {
        let _ = tx.send(FetchOutcome {
            status: 0,
            body: String::new(),
            error: Some("bridge closed".to_owned()),
        });
    }
}

/// [`Transport`] that routes each request through the native bridge instead of HTTP.
pub struct NativeTransport {
    inner: Arc<BridgeInner>,
    directory: String,
}

#[async_trait]
impl Transport for NativeTransport {
    fn base_url(&self) -> &str {
        "http://hya.internal"
    }

    fn directory(&self) -> &str {
        &self.directory
    }

    async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);

        let mut headers = serde_json::Map::new();
        headers.insert(
            crate::DIRECTORY_HEADER.to_owned(),
            Value::String(self.directory.clone()),
        );
        let body_str = match body {
            Some(value) => {
                headers.insert(
                    "content-type".to_owned(),
                    Value::String("application/json".to_owned()),
                );
                Some(serde_json::to_string(value)?)
            }
            None => None,
        };
        let frame = serde_json::json!({
            "type": "rpc.request",
            "id": id,
            "method": "fetch",
            "input": {
                "url": format!("http://hya.internal{path}"),
                "method": method,
                "headers": Value::Object(headers),
                "body": body_str,
            },
        });

        if let Err(err) = self.inner.write_frame(&frame).await {
            self.inner.pending.lock().await.remove(&id);
            return Err(err);
        }

        let outcome = rx
            .await
            .map_err(|_| SdkError::Bridge("bridge dropped the request".to_owned()))?;
        if let Some(err) = outcome.error {
            return Err(SdkError::Bridge(err));
        }
        if !(200..300).contains(&outcome.status) {
            return Err(SdkError::Http(format!(
                "status {} for {method} {path}",
                outcome.status
            )));
        }
        if outcome.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&outcome.body).map_err(|e| SdkError::Http(e.to_string()))
    }
}

/// Native-transport [`Client`]: the same surface as `HttpClient`, backed by the stdio bridge.
pub type NativeClient = ApiClient<NativeTransport>;
