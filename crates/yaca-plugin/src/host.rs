//! `PluginHost` — spawns every configured plugin, preserves declared load
//! order, runs the hook chains the bridges drive, and supervises each child:
//! a crash (EOF mid-call) marks the plugin `Dead`, the next call respawns it,
//! and exceeding the restart budget moves it to `Disabled`. Generalizes
//! `yaca_mcp::manager::McpManager`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use yaca_proto::{Envelope, SessionId, ToolCallId, WorkspaceAdapterInfo};
use yaca_tool::Tool;

use crate::client::{ChildGuard, PluginClient};
use crate::config::PluginSpec;
use crate::error::PluginError;
use crate::messages::{
    EventNotificationParams, HookName, HookPosture, HostInfo, METHOD_EVENT, METHOD_TOOL_CALL,
    ToolCallParams, ToolCallReply, ToolInfo,
};
use crate::plugin_tool::PluginTool;

mod connection;

const EVENT_CHANNEL_CAP: usize = 256;
const EVENT_DROP_WARN_EVERY: u64 = 256;
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    Alive,
    Dead,
    Disabled,
}

pub struct PluginHost {
    plugins: Vec<Arc<PluginConn>>,
}

struct LiveClient {
    client: PluginClient,
    _guard: ChildGuard,
}

pub(crate) struct PluginConn {
    pub(crate) id: String,
    pub(crate) hooks: HashMap<HookName, HookPosture>,
    pub(crate) tools: Vec<ToolInfo>,
    pub(crate) workspace_adapters: Vec<WorkspaceAdapterInfo>,
    pub(crate) timeout: Duration,
    command: Vec<String>,
    env: BTreeMap<String, String>,
    host_info: HostInfo,
    live: Mutex<Option<LiveClient>>,
    restarts: Mutex<Vec<Instant>>,
    disabled: AtomicBool,
    event_tx: mpsc::Sender<Envelope>,
    event_drops: AtomicU64,
}

impl PluginConn {
    pub(crate) fn posture(&self, hook: HookName) -> Option<HookPosture> {
        self.hooks.get(&hook).copied()
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
                if matches!(error, PluginError::Closed | PluginError::OversizedLine(_)) {
                    *self.live.lock().await = None;
                }
                Err(error)
            }
        }
    }

    pub(crate) async fn call_tool(
        &self,
        tool: &str,
        session: SessionId,
        call: ToolCallId,
        input: Value,
    ) -> Result<ToolCallReply, PluginError> {
        let client = self.ensure_client().await?;
        let params = serde_json::to_value(ToolCallParams {
            tool: tool.to_string(),
            session,
            call,
            input,
        })
        .map_err(|error| PluginError::Json(error.to_string()))?;
        match client.call(METHOD_TOOL_CALL, params, self.timeout).await {
            Ok(value) => {
                serde_json::from_value(value).map_err(|error| PluginError::Json(error.to_string()))
            }
            Err(error) => {
                if matches!(error, PluginError::Closed | PluginError::OversizedLine(_)) {
                    *self.live.lock().await = None;
                }
                Err(error)
            }
        }
    }

    pub(crate) async fn status(&self) -> PluginStatus {
        if self.disabled.load(Ordering::Relaxed) {
            PluginStatus::Disabled
        } else if self.live.lock().await.is_some() {
            PluginStatus::Alive
        } else {
            PluginStatus::Dead
        }
    }

    async fn ensure_client(&self) -> Result<PluginClient, PluginError> {
        if self.disabled.load(Ordering::Relaxed) {
            return Err(PluginError::Disabled);
        }
        let mut live = self.live.lock().await;
        if let Some(current) = live.as_ref() {
            return Ok(current.client.clone());
        }
        if !self.charge_restart().await {
            self.disabled.store(true, Ordering::Relaxed);
            tracing::warn!(plugin = %self.id, "plugin disabled after exceeding restart budget");
            return Err(PluginError::Disabled);
        }
        let env = (!self.env.is_empty()).then_some(&self.env);
        let (client, guard) = PluginClient::spawn(&self.command, env)?;
        client.initialize(self.host_info.clone()).await?;
        *live = Some(LiveClient {
            client: client.clone(),
            _guard: guard,
        });
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

    async fn notify_event(&self, envelope: Envelope) {
        let client = self
            .live
            .lock()
            .await
            .as_ref()
            .map(|live| live.client.clone());
        if let Some(client) = client {
            match serde_json::to_value(EventNotificationParams { envelope }) {
                Ok(params) => {
                    let _ = client.notify(METHOD_EVENT, params).await;
                }
                Err(error) => tracing::warn!(%error, "event notification serialize failed"),
            }
        }
    }
}

impl PluginHost {
    pub async fn connect_all(specs: Vec<PluginSpec>, host: HostInfo) -> Self {
        let mut set = tokio::task::JoinSet::new();
        for (index, spec) in specs.into_iter().enumerate() {
            let host = host.clone();
            set.spawn(async move { (index, connection::connect_one(spec, host).await) });
        }
        let mut collected: Vec<(usize, Arc<PluginConn>)> = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((index, Ok(conn))) => collected.push((index, conn)),
                Ok((index, Err(error))) => {
                    tracing::warn!(%error, index, "plugin unavailable");
                }
                Err(error) => tracing::warn!(%error, "plugin connect task failed"),
            }
        }
        collected.sort_by_key(|(index, _)| *index);
        let plugins = collected.into_iter().map(|(_, conn)| conn).collect();
        Self { plugins }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    #[must_use]
    pub fn plugin_ids(&self) -> Vec<String> {
        self.plugins.iter().map(|conn| conn.id.clone()).collect()
    }

    #[must_use]
    pub fn declared_tools(&self) -> Vec<(&str, &[ToolInfo])> {
        self.plugins
            .iter()
            .map(|conn| (conn.id.as_str(), conn.tools.as_slice()))
            .collect()
    }

    #[must_use]
    pub fn workspace_adapters(&self) -> Vec<WorkspaceAdapterInfo> {
        self.plugins
            .iter()
            .flat_map(|conn| conn.workspace_adapters.iter().cloned())
            .collect()
    }

    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.plugins
            .iter()
            .flat_map(|conn| {
                conn.tools
                    .iter()
                    .filter_map(|tool| PluginTool::try_new(conn.clone(), tool.clone()))
            })
            .collect()
    }

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
            if conn.hooks.contains_key(&HookName::Event) {
                conn.enqueue_event(envelope);
            }
        }
    }
}
