//! Stdio JSON-RPC MCP client: spawn a server process, initialize, call methods.
//!
//! Transport is one JSON object per line on the child stdin/stdout. Responses are
//! demultiplexed by request `id`. A held `ChildGuard` owns process lifetime.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};

use crate::protocol::{JsonRpcRequest, JsonRpcResponse};

/// Default timeout for ordinary MCP method calls (`tools/list`, `tools/call`, …).
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Shorter timeout used only for the `initialize` handshake.
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LINE_BYTES: usize = 1024 * 1024;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>>;

/// Failures from spawning, framing, timing out, or decoding MCP traffic.
#[derive(Error, Debug, Clone)]
pub enum McpError {
    /// Configured command vector was empty (no program to spawn).
    #[error("mcp command is empty")]
    EmptyCommand,
    /// Child process did not provide the required stdio pipe.
    #[error("mcp stdio unavailable: {0}")]
    MissingPipe(&'static str),
    /// Read/write/spawn OS error detail.
    #[error("io: {0}")]
    Io(String),
    /// Request or response JSON failed to (de)serialize.
    #[error("json: {0}")]
    Json(String),
    /// Server returned a JSON-RPC error object.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// Server error message.
        message: String,
    },
    /// No response within the call timeout.
    #[error("mcp call timed out: {method}")]
    Timeout {
        /// Method that timed out.
        method: String,
    },
    /// Stdout closed or pending map was cancelled.
    #[error("mcp connection closed")]
    Closed,
    /// A single stdout line exceeded the 1 MiB hard limit.
    #[error("mcp line exceeded 1048576 bytes")]
    OversizedLine,
}

/// Cloneable JSON-RPC client over an async reader/writer pair (usually child stdio).
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    next_id: AtomicU64,
    pending: Pending,
}

/// Owns a spawned MCP child process; on drop, SIGTERM (Unix) then kill and wait.
///
/// Keep this guard alive for as long as the paired [`McpClient`] should talk to
/// the process. Dropping it tears down the server even if client clones remain.
pub struct ChildGuard {
    child: StdMutex<Option<Child>>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let child = match self.child.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(mut child) = child {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        terminate_child(&mut child).await;
                    });
                }
                Err(_) => {
                    let _ = child.start_kill();
                }
            }
        }
    }
}

async fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    if let Some(id) = child.id() {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(id.to_string())
            .status()
            .await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let _ = child.start_kill();
    let _ = child.wait().await;
}

impl McpClient {
    /// Build a client from existing async pipes and start the stdout demux task.
    ///
    /// Used by tests and by [`Self::spawn`] after taking the child stdio handles.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        spawn_reader(reader, Arc::clone(&pending));
        Self {
            inner: Arc::new(ClientInner {
                writer: Mutex::new(Box::new(writer)),
                next_id: AtomicU64::new(1),
                pending,
            }),
        }
    }

    /// Spawn `command[0]` with `command[1..]` as args, optional env, piped stdio.
    ///
    /// Returns the client plus a `ChildGuard` that must be retained for the
    /// process lifetime. Stderr is discarded. Fails with [`McpError::EmptyCommand`]
    /// when `command` is empty.
    pub fn spawn(
        command: &[String],
        env: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<(Self, ChildGuard), McpError> {
        let (program, args) = command.split_first().ok_or(McpError::EmptyCommand)?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(env) = env {
            cmd.envs(env);
        }
        let mut child = cmd.spawn().map_err(|e| McpError::Io(e.to_string()))?;
        let stdout = child.stdout.take().ok_or(McpError::MissingPipe("stdout"))?;
        let stdin = child.stdin.take().ok_or(McpError::MissingPipe("stdin"))?;
        Ok((
            Self::new(stdout, stdin),
            ChildGuard {
                child: StdMutex::new(Some(child)),
            },
        ))
    }

    /// Send MCP `initialize` with hya client info; uses [`INITIALIZE_TIMEOUT`].
    pub async fn initialize(&self) -> Result<Value, McpError> {
        self.call(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "hya", "version": env!("CARGO_PKG_VERSION") }
            }),
            INITIALIZE_TIMEOUT,
        )
        .await
    }

    /// Send a JSON-RPC notification (no `id`, no response awaited). Used for the
    /// spec-required `notifications/initialized` handshake and other client → server
    /// notifications.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let line = serde_json::to_vec(&message).map_err(|e| McpError::Json(e.to_string()))?;
        let mut writer = self.inner.writer.lock().await;
        writer
            .write_all(&line)
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        Ok(())
    }

    /// Send a JSON-RPC request and await the matching response within `timeout`.
    ///
    /// On timeout the pending entry is removed; late responses for that id are dropped.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };
        let line = serde_json::to_vec(&request).map_err(|e| McpError::Json(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        {
            let mut writer = self.inner.writer.lock().await;
            writer
                .write_all(&line)
                .await
                .map_err(|e| McpError::Io(e.to_string()))?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|e| McpError::Io(e.to_string()))?;
            writer
                .flush()
                .await
                .map_err(|e| McpError::Io(e.to_string()))?;
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(McpError::Closed),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(McpError::Timeout {
                    method: method.to_string(),
                })
            }
        }
    }
}

fn spawn_reader<R>(reader: R, pending: Pending)
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let read = match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    close_pending(&pending, McpError::Io(e.to_string())).await;
                    return;
                }
            };
            if read > MAX_LINE_BYTES || buf.len() > MAX_LINE_BYTES {
                close_pending(&pending, McpError::OversizedLine).await;
                return;
            }
            let parsed = serde_json::from_slice::<JsonRpcResponse>(buf.trim_ascii_end())
                .map_err(|e| McpError::Json(e.to_string()));
            let response = match parsed {
                Ok(response) => response,
                Err(err) => {
                    close_pending(&pending, err).await;
                    return;
                }
            };
            if let Some(tx) = pending.lock().await.remove(&response.id) {
                let result = match (response.result, response.error) {
                    (Some(value), None) => Ok(value),
                    (_, Some(error)) => Err(McpError::Rpc {
                        code: error.code,
                        message: error.message,
                    }),
                    (None, None) => Err(McpError::Closed),
                };
                let _ = tx.send(result);
            }
        }
        close_pending(&pending, McpError::Closed).await;
    });
}

async fn close_pending(pending: &Pending, error: McpError) {
    let mut guard = pending.lock().await;
    let drained: Vec<_> = guard.drain().map(|(_, tx)| tx).collect();
    drop(guard);
    for tx in drained {
        let _ = tx.send(Err(error.clone()));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    #[tokio::test]
    async fn demuxes_responses_by_id() {
        let (client_io, server_io) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (server_read, mut server_write) = tokio::io::split(server_io);
        let client = McpClient::new(client_read, client_write);
        let server = tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            let first = lines.next_line().await.unwrap().unwrap();
            let second = lines.next_line().await.unwrap().unwrap();
            let first_req: JsonRpcRequest = serde_json::from_str(&first).unwrap();
            let second_req: JsonRpcRequest = serde_json::from_str(&second).unwrap();
            let second_response =
                json!({"jsonrpc":"2.0","id":second_req.id,"result":{"second":true}});
            let first_response = json!({"jsonrpc":"2.0","id":first_req.id,"result":{"first":true}});
            server_write
                .write_all(format!("{second_response}\n{first_response}\n").as_bytes())
                .await
                .unwrap();
        });

        let first = client.call("first", json!({}), DEFAULT_CALL_TIMEOUT);
        let second = client.call("second", json!({}), DEFAULT_CALL_TIMEOUT);
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap(), json!({"first": true}));
        assert_eq!(second.unwrap(), json!({"second": true}));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn notify_sends_notification_without_id() {
        let (client_io, server_io) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (server_read, _server_write) = tokio::io::split(server_io);
        let client = McpClient::new(client_read, client_write);
        let server = tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            lines.next_line().await.unwrap().unwrap()
        });
        client
            .notify("notifications/initialized", json!({}))
            .await
            .unwrap();
        let line = server.await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["method"], "notifications/initialized");
        assert_eq!(value["jsonrpc"], "2.0");
        assert!(value.get("id").is_none(), "notifications carry no id");
    }

    #[tokio::test]
    async fn returns_timeout_errors() {
        let (client_io, _server_io) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let client = McpClient::new(client_read, client_write);
        let result = client
            .call("slow", json!({}), Duration::from_millis(10))
            .await;
        assert!(matches!(result, Err(McpError::Timeout { method }) if method == "slow"));
    }
}
