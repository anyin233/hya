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

pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LINE_BYTES: usize = 1024 * 1024;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>>;

#[derive(Error, Debug, Clone)]
pub enum McpError {
    #[error("mcp command is empty")]
    EmptyCommand,
    #[error("mcp stdio unavailable: {0}")]
    MissingPipe(&'static str),
    #[error("io: {0}")]
    Io(String),
    #[error("json: {0}")]
    Json(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("mcp call timed out: {method}")]
    Timeout { method: String },
    #[error("mcp connection closed")]
    Closed,
    #[error("mcp line exceeded 1048576 bytes")]
    OversizedLine,
}

#[derive(Clone)]
pub struct McpClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    next_id: AtomicU64,
    pending: Pending,
}

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
