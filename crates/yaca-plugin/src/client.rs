//! Per-plugin RPC client over a child process's stdio (modeled on
//! `yaca_mcp::client`): id-correlated request/reply, per-call timeout, and a
//! `ChildGuard` that kills the child on drop.

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};

use crate::codec::MAX_LINE_BYTES;
use crate::error::PluginError;
use crate::messages::{
    HostInfo, InitializeParams, InitializeResult, METHOD_INITIALIZE, PROTOCOL_VERSION,
};
use crate::protocol::{Frame, JsonRpcNotification, JsonRpcRequest};

pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, PluginError>>>>>;

#[derive(Clone)]
pub struct PluginClient {
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
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.start_kill();
        }
    }
}

impl PluginClient {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        spawn_reader(reader, pending.clone());
        Self {
            inner: Arc::new(ClientInner {
                writer: Mutex::new(Box::new(writer)),
                next_id: AtomicU64::new(1),
                pending,
            }),
        }
    }

    /// # Errors
    /// `EmptyCommand` if `command` is empty, `Io` on spawn failure, or
    /// `MissingPipe` if the child's stdio could not be captured.
    pub fn spawn(
        command: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<(Self, ChildGuard), PluginError> {
        let (program, args) = command.split_first().ok_or(PluginError::EmptyCommand)?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(env) = env {
            cmd.envs(env);
        }
        let mut child = cmd.spawn().map_err(|e| PluginError::Io(e.to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(PluginError::MissingPipe("stdout"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or(PluginError::MissingPipe("stdin"))?;
        Ok((
            Self::new(stdout, stdin),
            ChildGuard {
                child: StdMutex::new(Some(child)),
            },
        ))
    }

    /// # Errors
    /// `Json` on a (de)serialization failure or the call-level errors from
    /// [`PluginClient::call`].
    pub async fn initialize(&self, host: HostInfo) -> Result<InitializeResult, PluginError> {
        let params = serde_json::to_value(InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            host,
        })
        .map_err(|e| PluginError::Json(e.to_string()))?;
        let value = self
            .call(METHOD_INITIALIZE, params, INITIALIZE_TIMEOUT)
            .await?;
        serde_json::from_value(value).map_err(|e| PluginError::Json(e.to_string()))
    }

    /// # Errors
    /// `Json` on serialize failure, `Io` on write failure, `Timeout` if no reply
    /// arrives in `timeout`, `Closed` if the channel ends, or `Rpc` on a plugin
    /// error reply.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, PluginError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let line = serde_json::to_vec(&JsonRpcRequest::new(id, method, params))
            .map_err(|e| PluginError::Json(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        self.write_line(&line).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(PluginError::Closed),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(PluginError::Timeout {
                    method: method.to_string(),
                })
            }
        }
    }

    /// # Errors
    /// `Json` on serialize failure or `Io` on write failure.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), PluginError> {
        let line = serde_json::to_vec(&JsonRpcNotification::new(method, params))
            .map_err(|e| PluginError::Json(e.to_string()))?;
        self.write_line(&line).await
    }

    async fn write_line(&self, line: &[u8]) -> Result<(), PluginError> {
        let mut writer = self.inner.writer.lock().await;
        writer
            .write_all(line)
            .await
            .map_err(|e| PluginError::Io(e.to_string()))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|e| PluginError::Io(e.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|e| PluginError::Io(e.to_string()))
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
                    close_pending(&pending, PluginError::Io(e.to_string())).await;
                    return;
                }
            };
            if read > MAX_LINE_BYTES || buf.len() > MAX_LINE_BYTES {
                close_pending(&pending, PluginError::OversizedLine(MAX_LINE_BYTES)).await;
                return;
            }
            let line = match std::str::from_utf8(buf.trim_ascii_end()) {
                Ok(line) => line,
                Err(e) => {
                    close_pending(&pending, PluginError::Json(e.to_string())).await;
                    return;
                }
            };
            if line.is_empty() {
                continue;
            }
            match Frame::parse(line) {
                Ok(Frame::Response(resp)) => {
                    if let Some(tx) = pending.lock().await.remove(&resp.id) {
                        let result = match (resp.result, resp.error) {
                            (Some(value), _) => Ok(value),
                            (None, Some(err)) => Err(PluginError::Rpc {
                                code: err.code,
                                message: err.message,
                            }),
                            (None, None) => Ok(Value::Null),
                        };
                        let _ = tx.send(result);
                    }
                }
                Ok(Frame::Notification(_)) | Ok(Frame::Request(_)) => {}
                Err(e) => {
                    close_pending(&pending, PluginError::Json(e)).await;
                    return;
                }
            }
        }
        close_pending(&pending, PluginError::Closed).await;
    });
}

async fn close_pending(pending: &Pending, error: PluginError) {
    let mut map = pending.lock().await;
    for (_, tx) in map.drain() {
        let _ = tx.send(Err(error.clone()));
    }
}
