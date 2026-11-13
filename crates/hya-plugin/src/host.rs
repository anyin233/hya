//! `PluginHost` — spawns every configured plugin, preserves declared load
//! order, runs the hook chains the bridges drive, and supervises each child:
//! a crash (EOF mid-call) marks the plugin `Dead`, the next call respawns it,
//! and exceeding the restart budget moves it to `Disabled`. Generalizes
//! `hya_mcp::manager::McpManager`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hya_proto::{Envelope, SessionId, ToolCallId};
use hya_tool::{Tool, ToolCtx};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

use crate::client::{ChildGuard, PluginClient};
use crate::config::PluginSpec;
use crate::error::PluginError;
use crate::messages::{
    EventNotificationParams, HookName, HostInfo, METHOD_EVENT, PluginContributionSet, ToolCallReply,
};

use crate::native_capability::BundleCapability;
use crate::plugin_tool::PluginTool;

mod connection;

const EVENT_CHANNEL_CAP: usize = 256;
const EVENT_DROP_WARN_EVERY: u64 = 256;
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);

/// Lifecycle status of one loaded plugin connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    /// Child is running and accepting calls.
    Alive,
    /// Child exited or EOF; next call may respawn within the restart budget.
    Dead,
    /// Respawned child changed its initialize declaration; no longer usable.
    DeclarationDrift,
    /// Restart budget exceeded; host will not respawn.
    Disabled,
}

/// Multi-plugin manager: connect-all, tools, hooks, and crash/restart policy.
pub struct PluginHost {
    plugins: Vec<Arc<PluginConn>>,
}

/// One successfully connected plugin, ready for tool registration and dispatch.
#[derive(Clone)]
pub struct PreparedPlugin {
    conn: Arc<PluginConn>,
}

impl PreparedPlugin {
    /// The same connection owner retained by tool adapters and hook dispatch.
    #[must_use]
    pub fn runtime_owner(&self) -> Arc<dyn hya_core::RuntimeSourceOwner> {
        self.conn.clone()
    }

    /// Stop this connection and join its I/O even while tools retain it.
    pub async fn shutdown(&self) -> Result<(), PluginError> {
        self.conn.shutdown(false).await
    }

    /// Transport-loss signal for a session-private activation. Shared sources respawn instead.
    #[must_use]
    pub fn loss_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        self.conn.activation_loss.clone()
    }

    /// Bind an exported tool under a consumer's canonical name.
    /// Selection changes the adapter only; the connection retains all exports.
    #[must_use]
    pub fn tool(&self, local: &str, canonical: &str) -> Option<Arc<dyn Tool>> {
        let info = self
            .conn
            .contributions
            .tools
            .iter()
            .find(|tool| tool.name == local)?;
        Some(PluginTool::named(
            self.conn.clone(),
            info.clone(),
            canonical,
        ))
    }

    /// Configured plugin id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.conn.id
    }

    /// Canonical bytes of the initialize declaration (identity / drift checks).
    #[must_use]
    pub fn canonical_declaration(&self) -> &[u8] {
        &self.conn.canonical_declaration
    }

    /// Typed tool, Skill, hook, and workspace-adapter declarations.
    #[must_use]
    pub fn contributions(&self) -> &PluginContributionSet {
        &self.conn.contributions
    }

    /// Tool adapters for this plugin's validated complete declaration.
    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.conn
            .contributions
            .tools
            .iter()
            .map(|tool| PluginTool::bind(self.conn.clone(), tool.clone()))
            .collect()
    }
}

impl hya_core::RuntimeSourceOwner for PluginConn {
    fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Err(error) = PluginConn::shutdown(self, true).await {
                tracing::warn!(%error, "stop plugin connection");
            }
        })
    }
}

struct LiveClient {
    client: PluginClient,
    guard: ChildGuard,
}

pub(crate) struct PluginConn {
    io_tasks: Option<tokio_util::task::TaskTracker>,
    pub(crate) id: String,
    pub(crate) hooks: HashSet<HookName>,
    pub(crate) contributions: PluginContributionSet,
    canonical_declaration: Arc<[u8]>,
    pub(crate) timeout: Duration,
    spec: PluginSpec,
    host_info: HostInfo,
    live: std::sync::Mutex<Option<LiveClient>>,
    connecting: Mutex<()>,
    restarts: Mutex<Vec<Instant>>,
    disabled: AtomicBool,
    stopped: AtomicBool,
    activation_loss: Option<tokio_util::sync::CancellationToken>,
    declaration_drift: AtomicBool,
    event_tx: mpsc::Sender<Envelope>,
    event_drops: AtomicU64,
    event_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Read-only host services behind bundle capabilities (bundle processes
    /// only; `None` for configured plugins and hosts without a store).
    host_reads: Option<Arc<dyn hya_core::HostSessionReads>>,
}

impl Drop for PluginConn {
    fn drop(&mut self) {
        if let Some(task) = self
            .event_task
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        if let Some(live) = self
            .live
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            live.guard.stop_now();
        }
    }
}

impl PluginConn {
    async fn shutdown(&self, force: bool) -> Result<(), PluginError> {
        self.stopped.store(true, Ordering::Relaxed);
        let _connecting = self.connecting.lock().await;
        let task = self
            .event_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        if let Some(mut live) = self.stop() {
            if force || live.client.is_closed() {
                live.guard.terminate().await?;
            } else {
                live.guard.shutdown().await?;
            }
        }
        Ok(())
    }

    pub(crate) fn is_activation(&self) -> bool {
        self.spec.is_activation()
    }

    pub(crate) fn activation_is_lost(&self) -> bool {
        !self.stopped.load(Ordering::Relaxed)
            && self
                .activation_loss
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
    }

    pub(crate) fn has_hook(&self, hook: HookName) -> bool {
        self.hooks.contains(&hook)
    }

    pub(crate) async fn call_hook(
        &self,
        hook: HookName,
        params: Value,
    ) -> Result<Value, PluginError> {
        let client = self.ensure_client().await?;
        match client.call(&hook.method(), params, self.timeout).await {
            Ok(value) => Ok(value),
            Err(error) => {
                self.invalidate_client(&client).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn call_tool(
        &self,
        ctx: &ToolCtx,
        tool: &str,
        session: SessionId,
        call: ToolCallId,
        input: Value,
    ) -> Result<ToolCallReply, PluginError> {
        let client = self.ensure_client().await?;
        // Every installed-bundle process (any kind) receives a call-scoped,
        // read-only / permission-checked capability; configured plugins never do.
        let reply = if self.spec.has_capabilities() {
            client
                .call_tool_with_capability_timeout(
                    tool,
                    session,
                    call,
                    input,
                    Arc::new(BundleCapability::tool_call(ctx, self.host_reads.clone())),
                    self.timeout,
                )
                .await
        } else {
            client
                .call_tool_with_timeout(tool, session, call, input, self.timeout)
                .await
        };
        match reply {
            Ok(reply) => Ok(reply),
            Err(error) => {
                self.invalidate_client(&client).await;
                Err(error)
            }
        }
    }

    /// Forward one routed bundle API request to this bundle process.
    pub(crate) async fn request_api(
        &self,
        request: hya_core::BundleApiRequest,
    ) -> Result<crate::messages::ApiRequestResult, PluginError> {
        if !self.spec.has_capabilities() {
            return Err(PluginError::Json(
                "only bundle processes serve API endpoints".to_string(),
            ));
        }
        let client = self.ensure_client().await?;
        let call = ToolCallId::new();
        let capability = Arc::new(BundleCapability::api(
            &request.api,
            request.session,
            call,
            self.host_reads.clone(),
        ));
        let params = crate::messages::ApiRequestParams {
            api: request.api,
            method: request.method.as_str().to_string(),
            path: request.path,
            path_params: request.path_params,
            query: request.query,
            body: request.body,
            session: request.session,
            call,
            host_capability: String::new(),
        };
        match client.request_api(params, capability, self.timeout).await {
            Ok(result) => Ok(result),
            Err(error) => {
                self.invalidate_client(&client).await;
                Err(error)
            }
        }
    }

    pub(crate) async fn status(&self) -> PluginStatus {
        if self.disabled.load(Ordering::Relaxed) {
            PluginStatus::Disabled
        } else if self.declaration_drift.load(Ordering::Relaxed) {
            PluginStatus::DeclarationDrift
        } else if self
            .lock_live()
            .as_ref()
            .is_some_and(|live| !live.client.is_closed())
        {
            PluginStatus::Alive
        } else {
            PluginStatus::Dead
        }
    }

    fn lock_live(&self) -> std::sync::MutexGuard<'_, Option<LiveClient>> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn invalidate_client(&self, used: &PluginClient) {
        if !used.is_closed() {
            return;
        }
        let _connecting = self.connecting.lock().await;
        let previous = {
            let mut live = self.lock_live();
            if live
                .as_ref()
                .is_some_and(|current| current.client.same_connection(used))
            {
                live.take()
            } else {
                None
            }
        };
        if let Some(mut previous) = previous
            && let Err(error) = previous.guard.terminate().await
        {
            tracing::warn!(%error, "reap closed plugin connection");
        }
    }

    fn stop(&self) -> Option<LiveClient> {
        self.stopped.store(true, Ordering::Relaxed);
        self.lock_live().take()
    }

    async fn ensure_client(&self) -> Result<PluginClient, PluginError> {
        let _connecting = self.connecting.lock().await;
        if self.disabled.load(Ordering::Relaxed) {
            return Err(PluginError::Disabled);
        }
        if self.declaration_drift.load(Ordering::Relaxed) {
            return Err(PluginError::DeclarationDrift {
                plugin: self.id.clone(),
            });
        }
        let previous = {
            let mut live = self.lock_live();
            if self.stopped.load(Ordering::Relaxed) {
                return Err(PluginError::Closed);
            }
            if let Some(current) = live.as_ref()
                && !current.client.is_closed()
            {
                return Ok(current.client.clone());
            }
            live.take()
        };
        if let Some(mut previous) = previous
            && let Err(error) = previous.guard.terminate().await
        {
            tracing::warn!(%error, "reap previous plugin connection");
        }
        // Session-private state must be recovered by the session supervisor.
        if self.spec.is_activation() {
            return Err(PluginError::Closed);
        }
        if !self.charge_restart().await {
            self.disabled.store(true, Ordering::Relaxed);
            tracing::warn!(plugin = %self.id, "plugin disabled after exceeding restart budget");
            return Err(PluginError::Disabled);
        }
        let (mut started, init) = hya_mcp::io_tasks::scope(
            self.io_tasks.clone(),
            connection::start_client(&self.spec, &self.host_info),
        )
        .await?;
        if canonical_initialize(&init)?.as_slice() != self.canonical_declaration.as_ref() {
            self.declaration_drift.store(true, Ordering::Relaxed);
            let _ = started.guard.shutdown().await;
            return Err(PluginError::DeclarationDrift {
                plugin: self.id.clone(),
            });
        }
        if self.stopped.load(Ordering::Relaxed) {
            let _ = started.guard.terminate().await;
            return Err(PluginError::Closed);
        }
        let client = started.client.clone();
        *self.lock_live() = Some(started);
        tracing::info!(plugin = %self.id, "plugin respawned");
        Ok(client)
    }

    async fn charge_restart(&self) -> bool {
        let mut restarts = self.restarts.lock().await;
        let now = Instant::now();
        restarts.retain(|stamp| now.duration_since(*stamp) < RESTART_WINDOW);
        if restarts.len() >= MAX_RESTARTS {
            return false;
        }
        restarts.push(now);
        true
    }

    fn enqueue_event(&self, envelope: &Envelope) {
        if self.event_tx.try_send(envelope.clone()).is_err() {
            let dropped = self.event_drops.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped % EVENT_DROP_WARN_EVERY == 1 {
                tracing::warn!(plugin = %self.id, dropped, "event backpressure; dropping notification");
            }
        }
    }

    async fn notify_event(client: PluginClient, envelope: Envelope) {
        match serde_json::to_value(EventNotificationParams { envelope }) {
            Ok(params) => {
                let _ = client.notify(METHOD_EVENT, params).await;
            }
            Err(error) => tracing::warn!(%error, "event notification serialize failed"),
        }
    }
}

fn validate_initialize(
    spec: &PluginSpec,
    init: &crate::messages::InitializeResult,
) -> Result<(), PluginError> {
    if init.protocol_version != crate::messages::PROTOCOL_VERSION {
        return Err(PluginError::ProtocolMismatch {
            expected: crate::messages::PROTOCOL_VERSION,
            got: init.protocol_version,
        });
    }
    if !spec.is_activation() && init.plugin.id != spec.id {
        return Err(PluginError::IdentityMismatch {
            expected: spec.id.clone(),
            got: init.plugin.id.clone(),
        });
    }
    if spec.is_activation() && init.plugin.kind != crate::messages::PluginKindWire::Bun {
        return Err(PluginError::Json(
            "bundle sidecar initialize declaration is incompatible".into(),
        ));
    }
    init.contributions.validate(&spec.id)
}

fn canonical_initialize(init: &crate::messages::InitializeResult) -> Result<Vec<u8>, PluginError> {
    let mut declaration = init.clone();
    declaration
        .contributions
        .hooks
        .sort_by_key(|hook| hook.name);
    declaration.contributions.tools.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.description.cmp(&right.description))
            .then_with(|| {
                canonical_json(&left.input_schema)
                    .to_string()
                    .cmp(&canonical_json(&right.input_schema).to_string())
            })
    });
    declaration.contributions.skills.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.content.cmp(&right.content))
            .then_with(|| left.digest.cmp(&right.digest))
    });
    declaration.contributions.apis.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.description.cmp(&right.description))
    });
    declaration
        .contributions
        .workspace_adapters
        .sort_by(|left, right| {
            (&left.r#type, &left.name, &left.description).cmp(&(
                &right.r#type,
                &right.name,
                &right.description,
            ))
        });
    let value =
        serde_json::to_value(declaration).map_err(|error| PluginError::Json(error.to_string()))?;
    serde_json::to_vec(&canonical_json(&value))
        .map_err(|error| PluginError::Json(error.to_string()))
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(left, _)| *left);
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical_json(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        scalar => scalar.clone(),
    }
}

impl PluginHost {
    /// Signal connections and kill synchronously, even when views survive.
    /// Retain handles so explicit shutdown can join the I/O and reap processes.
    pub fn stop_now(&self) {
        for conn in &self.plugins {
            conn.stopped.store(true, Ordering::Relaxed);
            if let Some(task) = conn
                .event_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            {
                task.abort();
            }
            if let Some(live) = conn.lock_live().as_mut() {
                live.guard.stop_now();
            }
        }
    }

    /// Gracefully stop and reap this owner's processes. Retained views cannot respawn them.
    pub async fn shutdown(&self) -> Result<(), PluginError> {
        self.stop(false).await
    }

    /// Terminate and reap this owner's processes without a shutdown handshake.
    pub async fn terminate(&self) -> Result<(), PluginError> {
        self.stop(true).await
    }

    async fn stop(&self, force: bool) -> Result<(), PluginError> {
        let mut first_error = None;
        for conn in &self.plugins {
            if let Err(error) = conn.shutdown(force).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn is_healthy(&self) -> bool {
        self.plugins.iter().all(|conn| {
            !conn.stopped.load(Ordering::Relaxed)
                && conn
                    .activation_loss
                    .as_ref()
                    .is_none_or(|token| !token.is_cancelled())
        })
    }

    /// Connect one descriptor, retaining its launch policy and process handle.
    /// `reads` backs the read-only capabilities of generation-owned bundle processes.
    pub async fn connect(
        spec: PluginSpec,
        host: HostInfo,
        reads: Option<Arc<dyn hya_core::HostSessionReads>>,
    ) -> Result<Self, PluginError> {
        let conn = connection::connect_one(spec, host, reads).await?;
        Ok(Self {
            plugins: vec![conn],
        })
    }

    /// API endpoints declared by this host's processes in their initialize
    /// replies.
    #[must_use]
    pub fn declared_apis(&self) -> Vec<crate::messages::ApiInfo> {
        self.plugins
            .iter()
            .flat_map(|conn| conn.contributions.apis.iter().cloned())
            .collect()
    }

    /// Connect every spec in parallel; failed plugins are logged and omitted.
    pub async fn connect_all(specs: Vec<PluginSpec>, host: HostInfo) -> Self {
        Self::connect_all_observed(specs, host).await.0
    }

    /// Like [`PluginHost::connect_all`], but also returns per-id connect failures.
    ///
    /// Successful plugins keep their original `specs` order. Failures map plugin
    /// id → [`PluginError`].
    pub async fn connect_all_observed(
        specs: Vec<PluginSpec>,
        host: HostInfo,
    ) -> (Self, BTreeMap<String, PluginError>) {
        let mut set = tokio::task::JoinSet::new();
        for (index, spec) in specs.into_iter().enumerate() {
            let host = host.clone();
            let id = spec.id.clone();
            set.spawn(hya_mcp::io_tasks::scope(
                hya_mcp::io_tasks::current(),
                async move { (index, id, connection::connect_one(spec, host, None).await) },
            ));
        }
        let mut collected: Vec<(usize, Arc<PluginConn>)> = Vec::new();
        let mut failures = BTreeMap::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((index, _, Ok(conn))) => collected.push((index, conn)),
                Ok((index, id, Err(error))) => {
                    tracing::warn!(%error, index, "plugin unavailable");
                    failures.insert(id, error);
                }
                Err(error) => tracing::warn!(%error, "plugin connect task failed"),
            }
        }
        collected.sort_by_key(|(index, _)| *index);
        let plugins = collected.into_iter().map(|(_, conn)| conn).collect();
        (Self { plugins }, failures)
    }

    /// Whether no plugins connected successfully.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Number of successfully connected plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Typed contribution sets per plugin id in declared load order.
    #[must_use]
    pub fn contributions(&self) -> Vec<(&str, &PluginContributionSet)> {
        self.plugins
            .iter()
            .map(|conn| (conn.id.as_str(), &conn.contributions))
            .collect()
    }

    /// All plugin tools as host [`Tool`] adapters.
    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.plugins
            .iter()
            .flat_map(|conn| {
                conn.contributions
                    .tools
                    .iter()
                    .map(|tool| PluginTool::bind(conn.clone(), tool.clone()))
            })
            .collect()
    }

    /// Connected plugins as [`PreparedPlugin`] handles (same order as connect).
    #[must_use]
    pub fn prepared_plugins(&self) -> Vec<PreparedPlugin> {
        self.plugins
            .iter()
            .map(|conn| PreparedPlugin { conn: conn.clone() })
            .collect()
    }

    /// Current lifecycle status for `id`, if that plugin is loaded.
    pub async fn plugin_status(&self, id: &str) -> Option<PluginStatus> {
        for conn in &self.plugins {
            if conn.id == id {
                return Some(conn.status().await);
            }
        }
        None
    }

    pub(crate) fn plugins(&self) -> &[Arc<PluginConn>] {
        &self.plugins
    }

    pub(crate) fn fan_out_event(&self, envelope: &Envelope) {
        for conn in &self.plugins {
            if conn.hooks.contains(&HookName::Event) {
                conn.enqueue_event(envelope);
            }
        }
    }
}

/// A bundle host forwards routed API requests to its (single) bundle process.
#[async_trait::async_trait]
impl hya_core::BundleApiProvider for PluginHost {
    async fn request(
        &self,
        request: hya_core::BundleApiRequest,
    ) -> Result<hya_core::BundleApiReply, String> {
        let conn = self
            .plugins
            .iter()
            .find(|conn| {
                conn.spec.has_capabilities()
                    && conn
                        .contributions
                        .apis
                        .iter()
                        .any(|declared| declared.name == request.api)
            })
            .ok_or_else(|| format!("no bundle process declares API endpoint `{}`", request.api))?;
        let result = conn
            .request_api(request)
            .await
            .map_err(|error| error.to_string())?;
        Ok(hya_core::BundleApiReply {
            status: result.status,
            body: result.body,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    use hya_proto::WorkspaceAdapterInfo;
    use serde_json::json;

    use super::canonical_initialize;
    use crate::messages::{
        HookName, HookRegistration, InitializeResult, PROTOCOL_VERSION, PluginContributionSet,
        PluginInfo, PluginKindWire, ToolInfo,
    };

    async fn control(listener: &TcpListener, mode: &str) -> BufReader<TcpStream> {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket
            .write_all(format!("{mode}\n").as_bytes())
            .await
            .unwrap();
        BufReader::new(socket)
    }

    async fn controlled_connection(
        timeout_ms: u64,
    ) -> (
        std::sync::Arc<super::PluginConn>,
        TcpListener,
        BufReader<TcpStream>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let script = r#"
import json, socket, sys, os, threading
control = socket.create_connection(('127.0.0.1', int(sys.argv[1]))).makefile('rw')
mode = control.readline().strip()
threading.Thread(target=lambda: (control.readline(), os._exit(1)), daemon=True).start()
for line in sys.stdin:
    req = json.loads(line)
    if req.get('method') == 'initialize':
        result = {'protocol_version': 1, 'plugin': {'id': 'controlled', 'version': '1', 'kind': 'rust'}, 'hooks': [{'name': 'tool.execute.before'}]}
    elif req.get('method') == 'shutdown':
        print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': {}}), flush=True)
        break
    elif mode == 'hold':
        control.write('call\n')
        control.flush()
        continue
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': result}), flush=True)
"#;
        let spec = crate::config::PluginSpec {
            id: "controlled".into(),
            kind: PluginKindWire::Rust,
            command: vec![
                "python3".into(),
                "-c".into(),
                script.into(),
                listener.local_addr().unwrap().port().to_string(),
            ],
            timeout_ms: Some(timeout_ms),
            env: Default::default(),
            plugin_dir: None,
            launch: crate::config::ProcessLaunch::Bundle {
                root: std::env::current_dir().unwrap(),
                activation: None,
            },
        };
        let (conn, control) = tokio::join!(
            super::connection::connect_one(
                spec,
                crate::messages::HostInfo {
                    name: "test".into(),
                    version: "1".into()
                },
                None
            ),
            control(&listener, "hold"),
        );
        (conn.unwrap(), listener, control)
    }

    #[tokio::test]
    async fn stopping_connection_joins_event_worker_with_retained_plugin() {
        let spec = crate::config::PluginSpec {
            id: "events".into(), kind: PluginKindWire::Rust,
            command: vec!["python3".into(), "-c".into(), r#"
import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 if r['method']=='initialize':
  result={'protocol_version':1,'plugin':{'id':'events','version':'1','kind':'rust'},'hooks':[{'name':'event'}]}
 else: result={}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#.into()],
            timeout_ms: Some(1000), env: Default::default(), plugin_dir: None, launch: Default::default(),
        };
        let conn = super::connection::connect_one(
            spec,
            crate::messages::HostInfo {
                name: "test".into(),
                version: "1".into(),
            },
            None,
        )
        .await
        .unwrap();
        let event = conn
            .event_task
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .abort_handle();
        let retained = super::PreparedPlugin { conn: conn.clone() };
        let host = super::PluginHost {
            plugins: vec![conn.clone()],
        };
        host.stop_now();
        tokio::time::timeout(std::time::Duration::from_secs(5), host.shutdown())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), retained.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert!(event.is_finished());
        assert!(conn.lock_live().is_none());
        tokio::time::timeout(std::time::Duration::from_secs(5), retained.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn concurrent_old_failures_do_not_invalidate_replacement() {
        concurrent_failed_calls(true).await;
    }

    #[tokio::test]
    async fn concurrent_timeout_failures_do_not_invalidate_replacement() {
        concurrent_failed_calls(false).await;
    }

    async fn concurrent_failed_calls(crash: bool) {
        let (conn, listener, mut child) =
            controlled_connection(if crash { 5000 } else { 100 }).await;
        let mut calls = (0..8)
            .map(|_| Box::pin(conn.call_hook(HookName::ToolExecuteBefore, json!({}))))
            .collect::<Vec<_>>();
        for call in &mut calls {
            poll_fn(|cx| {
                assert!(call.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        let mut line = String::new();
        for _ in 0..8 {
            child.read_line(&mut line).await.unwrap();
        }
        if crash {
            child.get_mut().write_all(b"exit\n").await.unwrap();
            assert!(matches!(
                calls.remove(0).await,
                Err(crate::PluginError::Closed)
            ));
        } else {
            assert!(matches!(
                calls.remove(0).await,
                Err(crate::PluginError::Timeout { .. })
            ));
        }
        let (fresh, _child) = tokio::join!(
            conn.call_hook(HookName::ToolExecuteBefore, json!({})),
            control(&listener, "reply"),
        );
        fresh.unwrap();
        for call in calls {
            assert!(matches!(call.await, Err(crate::PluginError::Closed)));
        }
        let status = conn.status().await;
        let restarts = conn.restarts.lock().await.len();
        let host = super::PluginHost {
            plugins: vec![conn],
        };
        host.terminate().await.unwrap();
        assert_eq!(
            status,
            super::PluginStatus::Alive,
            "stale failures must leave the replacement alive"
        );
        assert_eq!(restarts, 1, "one crash must cost one restart");
    }

    #[tokio::test]
    async fn ensure_client_replaces_timeout_tainted_client_before_dispatch() {
        let (conn, listener, mut child) = controlled_connection(100).await;
        let old = conn.ensure_client().await.unwrap();
        let mut call = Box::pin(old.call(
            "hook/tool.execute.before",
            json!({}),
            std::time::Duration::from_millis(100),
        ));
        poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        child.read_line(&mut String::new()).await.unwrap();
        assert!(matches!(
            call.await,
            Err(crate::PluginError::Timeout { .. })
        ));
        assert!(old.is_closed());
        let replacement = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(conn.ensure_client(), control(&listener, "reply"))
        })
        .await;
        let closed = replacement
            .as_ref()
            .map(|(client, _)| client.as_ref().unwrap().is_closed())
            .unwrap_or(true);
        let host = super::PluginHost {
            plugins: vec![conn],
        };
        host.terminate().await.unwrap();
        assert!(
            !closed,
            "ensure_client must replace a timeout-tainted client"
        );
    }

    fn declaration() -> InitializeResult {
        InitializeResult {
            protocol_version: PROTOCOL_VERSION,
            plugin: PluginInfo {
                id: "complete-declaration".to_string(),
                version: "1.0.0".to_string(),
                kind: PluginKindWire::Rust,
            },
            contributions: PluginContributionSet {
                hooks: vec![
                    HookRegistration {
                        name: HookName::PermissionAsk,
                    },
                    HookRegistration {
                        name: HookName::CommandExecuteBefore,
                    },
                ],
                tools: vec![
                    ToolInfo {
                        name: "zeta".to_string(),
                        description: "zeta tool".to_string(),
                        input_schema: json!({
                            "properties": { "second": { "type": "string" }, "first": { "type": "boolean" } },
                            "type": "object"
                        }),
                    },
                    ToolInfo {
                        name: "alpha".to_string(),
                        description: "alpha tool".to_string(),
                        input_schema: json!({ "type": "object" }),
                    },
                ],
                skills: Vec::new(),
                workspace_adapters: vec![
                    WorkspaceAdapterInfo {
                        r#type: "vcs".to_string(),
                        name: "zeta".to_string(),
                        description: "zeta adapter".to_string(),
                    },
                    WorkspaceAdapterInfo {
                        r#type: "vcs".to_string(),
                        name: "alpha".to_string(),
                        description: "alpha adapter".to_string(),
                    },
                ],
                apis: Vec::new(),
            },
        }
    }

    #[test]
    fn initialize_declaration_is_order_independent_and_complete() {
        let original = declaration();
        let mut reordered = original.clone();
        reordered.contributions.hooks.reverse();
        reordered.contributions.tools.reverse();
        reordered.contributions.workspace_adapters.reverse();

        reordered.contributions.tools[1].input_schema = json!({
            "type": "object",
            "properties": { "first": { "type": "boolean" }, "second": { "type": "string" } }
        });

        let canonical = canonical_initialize(&original).unwrap_or_default();
        assert_eq!(
            canonical,
            canonical_initialize(&reordered).unwrap_or_default()
        );

        let mut changed_hook = original.clone();
        changed_hook.contributions.hooks[0].name = HookName::SessionEnd;

        assert_ne!(
            canonical,
            canonical_initialize(&changed_hook).unwrap_or_default()
        );

        let mut changed_tool = original.clone();
        changed_tool.contributions.tools[0].description = "changed".to_string();

        assert_ne!(
            canonical,
            canonical_initialize(&changed_tool).unwrap_or_default()
        );

        let mut changed_adapter = original;
        changed_adapter.contributions.workspace_adapters[0].description = "changed".to_string();
        assert_ne!(
            canonical,
            canonical_initialize(&changed_adapter).unwrap_or_default()
        );
    }
}
