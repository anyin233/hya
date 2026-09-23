//! Per-plugin JSON-RPC client and child-process lifecycle.
//!
//! [`PluginClient`] speaks NDJSON requests/notifications over async stdio.
//! `ChildGuard` owns the spawned process, stderr tail, and shutdown/terminate.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::codec::read_bounded_line;
use crate::error::PluginError;
use crate::messages::{
    ActivationMetadata, HostCapabilityParams, HostInfo, InitializeParams, InitializeResult,
    METHOD_HOST_CAPABILITY, METHOD_INITIALIZE, METHOD_SHUTDOWN, METHOD_TOOL_CALL, METHOD_VIEW_GET,
    PROTOCOL_VERSION, ToolCallParams, ToolCallReply, ViewGetParams, ViewGetResult,
};
use crate::protocol::{
    Frame, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, codes,
};
use hya_proto::{SessionId, ToolCallId};

/// Default timeout for ordinary host→plugin requests (30s).
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for the `initialize` handshake (5s).
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for the `shutdown` request before the child is terminated (1s).
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const STDERR_TAIL_BYTES: usize = 64 * 1024;

type PendingEntries = HashMap<u64, oneshot::Sender<Result<Value, PluginError>>>;
type Pending = Arc<StdMutex<PendingEntries>>;
type Writer = Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;
type Capabilities = Arc<StdMutex<HashMap<String, RegisteredCapability>>>;

/// Host-owned operations available to one explicitly authorized request: a
/// bundle-process tool call or a `view/get` view request.
///
/// The transport validates the opaque token, session, and call id before this
/// handler sees the operation. Implementations must still enforce their own
/// operation-specific permission and resource checks.
#[async_trait]
pub trait HostCapabilityHandler: Send + Sync {
    /// Dispatch a validated call-scoped host operation.
    async fn handle(&self, method: &str, params: Value) -> Result<Value, JsonRpcError>;
}

#[derive(Clone)]
struct RegisteredCapability {
    session: SessionId,
    call: ToolCallId,
    handler: Arc<dyn HostCapabilityHandler>,
    cancelled: CancellationToken,
}

struct CapabilityLease {
    capabilities: Capabilities,
    token: String,
}

impl Drop for CapabilityLease {
    fn drop(&mut self) {
        if let Some(entry) = lock_capabilities(&self.capabilities).remove(&self.token) {
            entry.cancelled.cancel();
        }
    }
}

fn lock_capabilities(
    capabilities: &Capabilities,
) -> std::sync::MutexGuard<'_, HashMap<String, RegisteredCapability>> {
    match capabilities.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

struct PendingRegistration {
    pending: Pending,
    id: u64,
}

impl PendingRegistration {
    fn insert(
        pending: Pending,
        id: u64,
        sender: oneshot::Sender<Result<Value, PluginError>>,
    ) -> Self {
        lock_pending(&pending).insert(id, sender);
        Self { pending, id }
    }
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        lock_pending(&self.pending).remove(&self.id);
    }
}

fn lock_pending(pending: &Pending) -> std::sync::MutexGuard<'_, PendingEntries> {
    match pending.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Cloneable JSON-RPC client bound to one plugin's stdin/stdout.
///
/// Multiplexes concurrent calls by request id, with optional timeouts and a
/// closed-token for cancellation when the child dies.
#[derive(Clone)]
pub struct PluginClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    writer: Writer,
    next_id: AtomicU64,
    pending: Pending,
    capabilities: Capabilities,
    closed: Arc<std::sync::atomic::AtomicBool>,
    closed_token: CancellationToken,
    timeout_taints_closed: bool,
}

/// Owns a spawned plugin process and its paired [`PluginClient`].
///
/// Dropping without [`ChildGuard::shutdown`] / [`ChildGuard::terminate`] leaves
/// cleanup to the OS; prefer explicit shutdown for a clean `shutdown` RPC.
pub struct ChildGuard {
    child: StdMutex<Option<Child>>,
    client: PluginClient,
    stderr_tail: Option<StderrTail>,
    stderr_task: StdMutex<Option<StderrTask>>,
}

type StderrTail = Arc<StdMutex<Vec<u8>>>;
type StderrTask = JoinHandle<Result<(), std::io::Error>>;

#[derive(Clone, Copy)]
enum SpawnMode {
    Standard,
    Bundle,
}

#[derive(Serialize)]
struct ActivationInitializeParams {
    #[serde(flatten)]
    initialize: InitializeParams,
    #[serde(flatten)]
    activation: ActivationMetadata,
}

impl ChildGuard {
    /// OS process id of the child, if still held.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        let guard = match self.child.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_ref().and_then(Child::id)
    }

    /// Snapshot of the rolling stderr capture (up to the configured tail size).
    #[must_use]
    pub fn stderr_tail(&self) -> Vec<u8> {
        let Some(tail) = &self.stderr_tail else {
            return Vec::new();
        };
        match tail.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// # Errors
    /// Returns `Closed` if the child was already taken, or an I/O/transport
    /// error if shutdown, termination, or stderr collection fails.
    pub async fn shutdown(&mut self) -> Result<ExitStatus, PluginError> {
        let child = self.take_child();
        let stderr_task = self.take_stderr_task();
        let Some(mut child) = child else {
            self.client.mark_closed(PluginError::Closed).await;
            let _ = await_stderr_task(stderr_task).await;
            return Err(PluginError::Closed);
        };
        let shutdown_result = self
            .client
            .call(METHOD_SHUTDOWN, json!({}), SHUTDOWN_TIMEOUT)
            .await;
        let status_result = terminate_child(&mut child).await;
        self.client.mark_closed(PluginError::Closed).await;
        let stderr_result = await_stderr_task(stderr_task).await;

        let status = status_result?;
        stderr_result?;
        shutdown_result?;
        Ok(status)
    }

    /// # Errors
    /// Returns `Closed` if the child was already taken, or an I/O error if
    /// termination, reaping, or stderr collection fails.
    pub async fn terminate(&mut self) -> Result<ExitStatus, PluginError> {
        let child = self.take_child();
        let stderr_task = self.take_stderr_task();
        let Some(mut child) = child else {
            self.client.mark_closed(PluginError::Closed).await;
            let _ = await_stderr_task(stderr_task).await;
            return Err(PluginError::Closed);
        };

        let kill_result = child
            .start_kill()
            .map_err(|error| PluginError::Io(error.to_string()));
        let status_result = child
            .wait()
            .await
            .map_err(|error| PluginError::Io(error.to_string()));
        self.client.mark_closed(PluginError::Closed).await;
        let stderr_result = await_stderr_task(stderr_task).await;

        kill_result?;
        let status = status_result?;
        stderr_result?;
        Ok(status)
    }

    /// # Errors
    /// Returns `Closed` if the child was already taken, or an I/O error if
    /// waiting or stderr collection fails.
    pub async fn wait_for_exit(&mut self) -> Result<ExitStatus, PluginError> {
        let child = self.take_child();
        let stderr_task = self.take_stderr_task();
        let Some(mut child) = child else {
            self.client.mark_closed(PluginError::Closed).await;
            let _ = await_stderr_task(stderr_task).await;
            return Err(PluginError::Closed);
        };

        let status_result = child
            .wait()
            .await
            .map_err(|error| PluginError::Io(error.to_string()));
        self.client.mark_closed(PluginError::Closed).await;
        let stderr_result = await_stderr_task(stderr_task).await;

        let status = status_result?;
        stderr_result?;
        Ok(status)
    }

    fn take_child(&self) -> Option<Child> {
        match self.child.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    fn take_stderr_task(&self) -> Option<StderrTask> {
        match self.stderr_task.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let child = self.take_child();
        let stderr_task = self.take_stderr_task();
        match child {
            Some(mut child) => {
                let client = self.client.clone();
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        handle.spawn(async move {
                            let _ = client
                                .call(METHOD_SHUTDOWN, json!({}), SHUTDOWN_TIMEOUT)
                                .await;
                            let _ = terminate_child(&mut child).await;
                            client.mark_closed(PluginError::Closed).await;
                            let _ = await_stderr_task(stderr_task).await;
                        });
                    }
                    Err(_) => {
                        let _ = child.start_kill();
                        if let Some(task) = stderr_task {
                            task.abort();
                        }
                    }
                }
            }
            None => {
                if let Some(task) = stderr_task {
                    let client = self.client.clone();
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            handle.spawn(async move {
                                client.mark_closed(PluginError::Closed).await;
                                let _ = await_stderr_task(Some(task)).await;
                            });
                        }
                        Err(_) => task.abort(),
                    }
                }
            }
        }
    }
}

async fn terminate_child(child: &mut Child) -> Result<ExitStatus, PluginError> {
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(_)) | Err(_) => {
            let _ = child.start_kill();
            child
                .wait()
                .await
                .map_err(|error| PluginError::Io(error.to_string()))
        }
    }
}

async fn await_stderr_task(task: Option<StderrTask>) -> Result<(), PluginError> {
    let Some(task) = task else {
        return Ok(());
    };
    match task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(PluginError::Io(error.to_string())),
        Err(error) => Err(PluginError::Io(error.to_string())),
    }
}

impl PluginClient {
    /// Build a client over already-connected reader/writer halves (tests and custom pipes).
    ///
    /// Spawns a background reader task that completes pending request futures.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::new_with_timeout_policy(reader, writer, false)
    }

    fn new_with_timeout_policy<R, W>(reader: R, writer: W, timeout_taints_closed: bool) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let capabilities: Capabilities = Arc::new(StdMutex::new(HashMap::new()));
        let writer: Writer = Arc::new(Mutex::new(Box::new(writer)));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closed_token = CancellationToken::new();
        spawn_reader(
            reader,
            writer.clone(),
            pending.clone(),
            capabilities.clone(),
            closed.clone(),
            closed_token.clone(),
            timeout_taints_closed,
        );
        Self {
            inner: Arc::new(ClientInner {
                writer,
                next_id: AtomicU64::new(1),
                pending,
                capabilities,
                closed,
                closed_token,
                timeout_taints_closed,
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
        Self::spawn_with_options(command, env, None, false, false, SpawnMode::Standard)
    }

    /// Spawns a bundle plugin in its activation directory while retaining a
    /// bounded stderr tail and only the explicitly supplied host environment.
    ///
    /// # Errors
    /// `EmptyCommand` if `command` is empty, `Io` on spawn failure, or
    /// `MissingPipe` if the child's stdio could not be captured.
    pub fn spawn_bundle(
        command: &[String],
        cwd: &Path,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<(Self, ChildGuard), PluginError> {
        Self::spawn_with_options(command, env, Some(cwd), true, true, SpawnMode::Bundle)
    }

    fn spawn_with_options(
        command: &[String],
        env: Option<&BTreeMap<String, String>>,
        cwd: Option<&Path>,
        capture_stderr: bool,
        timeout_taints_closed: bool,
        mode: SpawnMode,
    ) -> Result<(Self, ChildGuard), PluginError> {
        let (program, args) = command.split_first().ok_or(PluginError::EmptyCommand)?;
        let mut cmd = Command::new(program);
        if matches!(mode, SpawnMode::Bundle) {
            cmd.env_clear();
        }
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if capture_stderr {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .kill_on_drop(true);
        if let Some(env) = env {
            cmd.envs(env);
        }
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
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
        let stderr = if capture_stderr {
            Some(
                child
                    .stderr
                    .take()
                    .ok_or(PluginError::MissingPipe("stderr"))?,
            )
        } else {
            None
        };
        let client = Self::new_with_timeout_policy(stdout, stdin, timeout_taints_closed);
        let (stderr_tail, stderr_task) = match stderr {
            Some(stderr) => {
                let tail = Arc::new(StdMutex::new(Vec::new()));
                let task = spawn_stderr_reader(stderr, tail.clone());
                (Some(tail), Some(task))
            }
            None => (None, None),
        };
        let guard = ChildGuard {
            child: StdMutex::new(Some(child)),
            client: client.clone(),
            stderr_tail,
            stderr_task: StdMutex::new(stderr_task),
        };
        Ok((client, guard))
    }

    /// # Errors
    /// `Json` on a (de)serialization failure or the call-level errors from
    /// [`PluginClient::call`].
    pub async fn initialize(&self, host: HostInfo) -> Result<InitializeResult, PluginError> {
        self.initialize_with_params(InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            host,
        })
        .await
    }

    /// # Errors
    /// `Json` on a (de)serialization failure or the call-level errors from
    /// [`PluginClient::call`].
    pub async fn initialize_activation(
        &self,
        host: HostInfo,
        activation: ActivationMetadata,
    ) -> Result<InitializeResult, PluginError> {
        self.initialize_with_params(ActivationInitializeParams {
            initialize: InitializeParams {
                protocol_version: PROTOCOL_VERSION,
                host,
            },
            activation,
        })
        .await
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
        if self.is_closed() {
            return Err(PluginError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let line = serde_json::to_vec(&JsonRpcRequest::new(id, method, params))
            .map_err(|e| PluginError::Json(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        let _pending_registration = PendingRegistration::insert(self.inner.pending.clone(), id, tx);
        let operation = async {
            if let Err(error) = self.write_line(&line).await {
                self.mark_closed(error.clone()).await;
                return Err(error);
            }
            match rx.await {
                Ok(result) => result,
                Err(_) => Err(PluginError::Closed),
            }
        };
        match tokio::time::timeout(timeout, operation).await {
            Ok(result) => result,
            Err(_) => {
                if self.inner.timeout_taints_closed {
                    self.mark_closed(PluginError::Closed).await;
                }
                Err(PluginError::Timeout {
                    method: method.to_string(),
                })
            }
        }
    }

    /// Calls a Bundle-local tool through the existing request/reply method.
    ///
    /// # Errors
    /// `Json` on serialization/deserialization failure or the call-level
    /// errors from [`PluginClient::call`].
    pub async fn call_tool(
        &self,
        tool: &str,
        session: SessionId,
        call: ToolCallId,
        input: Value,
    ) -> Result<ToolCallReply, PluginError> {
        self.call_tool_with_timeout(tool, session, call, input, DEFAULT_CALL_TIMEOUT)
            .await
    }

    /// Call a native tool with a temporary, session-bound reverse-RPC authority.
    ///
    /// The authority is revoked on success, error, timeout, and caller
    /// cancellation. Ordinary plugin tool calls do not receive this field.
    ///
    /// # Errors
    /// Returns a plugin transport or JSON error as for [`Self::call_tool`].
    pub async fn call_tool_with_capability(
        &self,
        tool: &str,
        session: SessionId,
        call: ToolCallId,
        input: Value,
        handler: Arc<dyn HostCapabilityHandler>,
    ) -> Result<ToolCallReply, PluginError> {
        self.call_tool_with_capability_timeout(
            tool,
            session,
            call,
            input,
            handler,
            DEFAULT_CALL_TIMEOUT,
        )
        .await
    }

    pub(crate) async fn call_tool_with_capability_timeout(
        &self,
        tool: &str,
        session: SessionId,
        call: ToolCallId,
        input: Value,
        handler: Arc<dyn HostCapabilityHandler>,
        timeout: Duration,
    ) -> Result<ToolCallReply, PluginError> {
        let lease = self.register_capability(session, call, handler)?;
        let result = self
            .call_tool_with_timeout_and_capability(
                tool,
                session,
                call,
                input,
                timeout,
                Some(lease.token.clone()),
            )
            .await;
        drop(lease);
        result
    }

    /// Ask the plugin for one declared read-only session view.
    ///
    /// A fresh synthetic request id is minted and a capability lease bound to
    /// `(this connection, session, request id)` is handed to the plugin as
    /// `host_capability`; it is revoked on reply, error, timeout, and caller
    /// cancellation, exactly like a tool-call lease.
    ///
    /// # Errors
    /// Returns the call-level errors from [`Self::call`] or `Json` when the
    /// reply is not a `{ "body": <json> }` object.
    pub async fn get_view(
        &self,
        view: &str,
        session: SessionId,
        query: BTreeMap<String, String>,
        handler: Arc<dyn HostCapabilityHandler>,
        timeout: Duration,
    ) -> Result<ViewGetResult, PluginError> {
        let call = ToolCallId::new();
        let lease = self.register_capability(session, call, handler)?;
        let params = serde_json::to_value(ViewGetParams {
            view: view.to_string(),
            session,
            call,
            query,
            host_capability: lease.token.clone(),
        })
        .map_err(|error| PluginError::Json(error.to_string()))?;
        let result = self.call(METHOD_VIEW_GET, params, timeout).await;
        drop(lease);
        serde_json::from_value(result?).map_err(|error| PluginError::Json(error.to_string()))
    }

    fn register_capability(
        &self,
        session: SessionId,
        call: ToolCallId,
        handler: Arc<dyn HostCapabilityHandler>,
    ) -> Result<CapabilityLease, PluginError> {
        if self.is_closed() {
            return Err(PluginError::Closed);
        }
        let mut capabilities = lock_capabilities(&self.inner.capabilities);
        let token = loop {
            let candidate = ToolCallId::new().to_string();
            if !capabilities.contains_key(&candidate) {
                break candidate;
            }
        };
        capabilities.insert(
            token.clone(),
            RegisteredCapability {
                session,
                call,
                handler,
                cancelled: CancellationToken::new(),
            },
        );
        Ok(CapabilityLease {
            capabilities: self.inner.capabilities.clone(),
            token,
        })
    }

    pub(crate) async fn call_tool_with_timeout(
        &self,
        tool: &str,
        session: SessionId,
        call: ToolCallId,
        input: Value,
        timeout: Duration,
    ) -> Result<ToolCallReply, PluginError> {
        self.call_tool_with_timeout_and_capability(tool, session, call, input, timeout, None)
            .await
    }

    async fn call_tool_with_timeout_and_capability(
        &self,
        tool: &str,
        session: SessionId,
        call: ToolCallId,
        input: Value,
        timeout: Duration,
        host_capability: Option<String>,
    ) -> Result<ToolCallReply, PluginError> {
        let params = serde_json::to_value(ToolCallParams {
            tool: tool.to_string(),
            session,
            call,
            input,
            host_capability,
        })
        .map_err(|error| PluginError::Json(error.to_string()))?;
        let value = self.call(METHOD_TOOL_CALL, params, timeout).await?;
        serde_json::from_value(value).map_err(|error| PluginError::Json(error.to_string()))
    }

    /// # Errors
    /// `Json` on serialize failure or `Io` on write failure.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), PluginError> {
        if self.is_closed() {
            return Err(PluginError::Closed);
        }
        let line = serde_json::to_vec(&JsonRpcNotification::new(method, params))
            .map_err(|e| PluginError::Json(e.to_string()))?;
        match self.write_line(&line).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.mark_closed(error.clone()).await;
                Err(error)
            }
        }
    }

    async fn write_line(&self, line: &[u8]) -> Result<(), PluginError> {
        if self.is_closed() {
            return Err(PluginError::Closed);
        }
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

    /// Whether this client has been marked closed (EOF, crash, or explicit mark).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Cancellation token fired when the client is marked closed.
    #[must_use]
    pub fn closed_token(&self) -> CancellationToken {
        self.inner.closed_token.clone()
    }

    async fn mark_closed(&self, error: PluginError) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.closed_token.cancel();
        close_pending(&self.inner.pending, error);
    }

    async fn initialize_with_params<T: Serialize>(
        &self,
        params: T,
    ) -> Result<InitializeResult, PluginError> {
        let params = serde_json::to_value(params).map_err(|e| PluginError::Json(e.to_string()))?;
        let value = self
            .call(METHOD_INITIALIZE, params, INITIALIZE_TIMEOUT)
            .await?;
        let result = serde_json::from_value::<InitializeResult>(value)
            .map_err(|error| PluginError::Json(error.to_string()))?;
        result.contributions.validate(&result.plugin.id)?;
        Ok(result)
    }
}

fn spawn_reader<R>(
    reader: R,
    writer: Writer,
    pending: Pending,
    capabilities: Capabilities,
    closed: Arc<std::sync::atomic::AtomicBool>,
    closed_token: CancellationToken,
    strict_protocol: bool,
) where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            match read_bounded_line(&mut reader, &mut buf).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => {
                    close_transport(&closed, &closed_token, &pending, error).await;
                    return;
                }
            }
            let line = match std::str::from_utf8(buf.trim_ascii_end()) {
                Ok(line) => line,
                Err(e) => {
                    close_transport(
                        &closed,
                        &closed_token,
                        &pending,
                        PluginError::Json(e.to_string()),
                    )
                    .await;
                    return;
                }
            };
            if line.is_empty() {
                if strict_protocol {
                    close_transport(
                        &closed,
                        &closed_token,
                        &pending,
                        PluginError::Json("blank child stdout".to_string()),
                    )
                    .await;
                    return;
                }
                continue;
            }
            match Frame::parse(line) {
                Ok(Frame::Response(resp)) => {
                    if let Some(tx) = lock_pending(&pending).remove(&resp.id) {
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
                Ok(Frame::Notification(_)) => {
                    close_transport(
                        &closed,
                        &closed_token,
                        &pending,
                        PluginError::Json("unexpected child notification".to_string()),
                    )
                    .await;
                    return;
                }
                Ok(Frame::Request(request)) => {
                    if request.method != METHOD_HOST_CAPABILITY {
                        close_transport(
                            &closed,
                            &closed_token,
                            &pending,
                            PluginError::Json("unexpected child request".to_string()),
                        )
                        .await;
                        return;
                    }
                    let writer = writer.clone();
                    let pending = pending.clone();
                    let capabilities = capabilities.clone();
                    let closed = closed.clone();
                    let closed_token = closed_token.clone();
                    tokio::spawn(async move {
                        let response =
                            dispatch_host_capability(request, &capabilities, &closed_token).await;
                        if let Err(error) = write_host_response(&writer, response).await {
                            close_transport(&closed, &closed_token, &pending, error).await;
                        }
                    });
                }
                Err(e) => {
                    close_transport(&closed, &closed_token, &pending, PluginError::Json(e)).await;
                    return;
                }
            }
        }
        close_transport(&closed, &closed_token, &pending, PluginError::Closed).await;
    });
}

async fn dispatch_host_capability(
    request: JsonRpcRequest,
    capabilities: &Capabilities,
    closed_token: &CancellationToken,
) -> JsonRpcResponse {
    let params: HostCapabilityParams = match serde_json::from_value(request.params) {
        Ok(params) => params,
        Err(error) => {
            return JsonRpcResponse::err(request.id, codes::INVALID_PARAMS, error.to_string());
        }
    };
    let entry = lock_capabilities(capabilities)
        .get(&params.capability)
        .cloned();
    let Some(entry) =
        entry.filter(|entry| entry.session == params.session && entry.call == params.call)
    else {
        return JsonRpcResponse::err(
            request.id,
            codes::CAPABILITY_DENIED,
            "capability is not active for this call",
        );
    };
    let result = tokio::select! {
        biased;
        () = entry.cancelled.cancelled() => Err(JsonRpcError {
            code: codes::CAPABILITY_DENIED,
            message: "capability expired".to_string(),
            data: None,
        }),
        () = closed_token.cancelled() => Err(JsonRpcError {
            code: codes::CAPABILITY_DENIED,
            message: "plugin connection closed".to_string(),
            data: None,
        }),
        result = entry.handler.handle(&params.method, params.params) => result,
    };
    match result {
        Ok(value) => JsonRpcResponse::ok(request.id, value),
        Err(error) => JsonRpcResponse {
            jsonrpc: crate::protocol::JSONRPC_VERSION.to_string(),
            id: request.id,
            result: None,
            error: Some(error),
        },
    }
}

async fn write_host_response(
    writer: &Writer,
    response: JsonRpcResponse,
) -> Result<(), PluginError> {
    let line =
        serde_json::to_vec(&response).map_err(|error| PluginError::Json(error.to_string()))?;
    let mut writer = writer.lock().await;
    writer
        .write_all(&line)
        .await
        .map_err(|error| PluginError::Io(error.to_string()))?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|error| PluginError::Io(error.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|error| PluginError::Io(error.to_string()))
}

fn spawn_stderr_reader<R>(mut reader: R, tail: StderrTail) -> StderrTask
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0_u8; 8192];
        loop {
            let read = reader.read(&mut buf).await?;
            if read == 0 {
                return Ok(());
            }
            append_stderr_tail(&tail, &buf[..read]);
        }
    })
}

fn append_stderr_tail(tail: &StderrTail, chunk: &[u8]) {
    let mut tail = match tail.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if chunk.len() >= STDERR_TAIL_BYTES {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - STDERR_TAIL_BYTES..]);
        return;
    }
    let excess = tail
        .len()
        .saturating_add(chunk.len())
        .saturating_sub(STDERR_TAIL_BYTES);
    if excess > 0 {
        tail.drain(..excess);
    }
    tail.extend_from_slice(chunk);
}

fn close_pending(pending: &Pending, error: PluginError) {
    let mut map = lock_pending(pending);
    for (_, tx) in map.drain() {
        let _ = tx.send(Err(error.clone()));
    }
}

async fn close_transport(
    closed: &std::sync::atomic::AtomicBool,
    closed_token: &CancellationToken,
    pending: &Pending,
    error: PluginError,
) {
    if !closed.swap(true, Ordering::SeqCst) {
        closed_token.cancel();
        close_pending(pending, error);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use crate::error::PluginError;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    use super::{PluginClient, lock_pending};

    #[tokio::test]
    async fn dropping_inflight_call_removes_pending_entry() {
        let (_server_to_client, client_reader) = duplex(1024);
        let (client_writer, server_reader) = duplex(1024);
        let client = PluginClient::new(client_reader, client_writer);
        let call_client = client.clone();
        let call_task = tokio::spawn(async move {
            call_client
                .call("test/method", json!({}), std::time::Duration::from_secs(60))
                .await
        });

        let mut lines = BufReader::new(server_reader).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["method"], "test/method");
        assert!(request["id"].is_u64());
        assert_eq!(lock_pending(&client.inner.pending).len(), 1);

        call_task.abort();
        let _ = call_task.await;
        assert!(lock_pending(&client.inner.pending).is_empty());
    }

    #[tokio::test]
    async fn close_signal_fires_after_successful_reply_followed_by_eof() {
        let (mut server_writer, client_reader) = duplex(1024);
        let (client_writer, server_reader) = duplex(1024);
        let client = PluginClient::new(client_reader, client_writer);
        let call_client = client.clone();
        let call_task = tokio::spawn(async move {
            call_client
                .call("test/method", json!({}), std::time::Duration::from_secs(60))
                .await
        });

        let mut lines = BufReader::new(server_reader).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let response = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {"ok": true},
        }))
        .unwrap();
        server_writer.write_all(&response).await.unwrap();
        server_writer.write_all(b"\n").await.unwrap();
        server_writer.shutdown().await.unwrap();

        let result = call_task.await.unwrap().unwrap();
        assert_eq!(result, json!({"ok": true}));

        let closed = client.closed_token();
        tokio::time::timeout(std::time::Duration::from_millis(250), closed.cancelled())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn bundle_call_timeout_covers_blocked_request_write() {
        let (_reader_peer, client_reader) = duplex(1);
        let (client_writer, _writer_peer) = duplex(1);
        let client = PluginClient::new_with_timeout_policy(client_reader, client_writer, true);
        let method = "bundle/blocked-write";
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            client.call(
                method,
                json!({"payload": "x".repeat(4096)}),
                std::time::Duration::from_millis(20),
            ),
        )
        .await
        .unwrap();

        assert!(matches!(
            result,
            Err(PluginError::Timeout { method: timed_out_method })
                if timed_out_method == method
        ));
        assert!(client.is_closed());
    }

    #[tokio::test]
    async fn bundle_transport_rejects_child_originated_request() {
        let (mut server_writer, client_reader) = duplex(1024);
        let (client_writer, _server_reader) = duplex(1024);
        let client = PluginClient::new_with_timeout_policy(client_reader, client_writer, true);
        let request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "child/request",
            "params": {"value": true},
        }))
        .unwrap();

        server_writer.write_all(&request).await.unwrap();
        server_writer.write_all(b"\n").await.unwrap();

        let closed = client.closed_token();
        tokio::time::timeout(std::time::Duration::from_millis(250), closed.cancelled())
            .await
            .unwrap();
        assert!(client.is_closed());
    }

    #[tokio::test]
    async fn bundle_transport_rejects_child_originated_notification() {
        let (mut server_writer, client_reader) = duplex(1024);
        let (client_writer, _server_reader) = duplex(1024);
        let client = PluginClient::new_with_timeout_policy(client_reader, client_writer, true);
        let notification = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "child/notification",
            "params": {"value": true},
        }))
        .unwrap();

        server_writer.write_all(&notification).await.unwrap();
        server_writer.write_all(b"\n").await.unwrap();

        let closed = client.closed_token();
        tokio::time::timeout(std::time::Duration::from_millis(250), closed.cancelled())
            .await
            .unwrap();
        assert!(client.is_closed());
    }

    #[tokio::test]
    async fn bundle_transport_rejects_blank_stdout_frame() {
        let (mut server_writer, client_reader) = duplex(1024);
        let (client_writer, _server_reader) = duplex(1024);
        let client = PluginClient::new_with_timeout_policy(client_reader, client_writer, true);

        server_writer.write_all(b"\n").await.unwrap();

        let closed = client.closed_token();
        tokio::time::timeout(std::time::Duration::from_millis(250), closed.cancelled())
            .await
            .unwrap();
        assert!(client.is_closed());
    }
}
