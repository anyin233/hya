//! `PluginHost` — spawns every configured plugin, preserves declared load
//! order, runs the hook chains the bridges drive, and supervises each child:
//! a crash (EOF mid-call) marks the plugin `Dead`, the next call respawns it,
//! and exceeding the restart budget moves it to `Disabled`. Generalizes
//! `yaca_mcp::manager::McpManager`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use yaca_proto::Envelope;

use crate::client::{ChildGuard, DEFAULT_CALL_TIMEOUT, PluginClient};
use crate::config::PluginSpec;
use crate::error::PluginError;
use crate::messages::{
    EventNotificationParams, HookName, HookPosture, HostInfo, METHOD_EVENT, PROTOCOL_VERSION,
    ToolInfo,
};

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
            set.spawn(async move { (index, connect_one(spec, host).await) });
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

async fn connect_one(spec: PluginSpec, host: HostInfo) -> Result<Arc<PluginConn>, PluginError> {
    let timeout = spec
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CALL_TIMEOUT);
    let spawn_env = (!spec.env.is_empty()).then_some(&spec.env);
    let (client, guard) = PluginClient::spawn(&spec.command, spawn_env)?;
    let init = client.initialize(host.clone()).await?;
    if init.protocol_version != PROTOCOL_VERSION {
        return Err(PluginError::ProtocolMismatch {
            expected: PROTOCOL_VERSION,
            got: init.protocol_version,
        });
    }
    let mut hooks = HashMap::new();
    for registration in &init.hooks {
        let default = registration.name.default_posture();
        let declared = registration
            .posture
            .or_else(|| spec.posture_overrides.get(&registration.name).copied())
            .unwrap_or(default);
        hooks.insert(registration.name, force_safer(declared, default));
    }
    let has_event_hook = hooks.contains_key(&HookName::Event);
    let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAP);
    let conn = Arc::new(PluginConn {
        id: spec.id,
        hooks,
        tools: init.tools,
        timeout,
        command: spec.command,
        env: spec.env,
        host_info: host,
        live: Mutex::new(Some(LiveClient {
            client,
            _guard: guard,
        })),
        restarts: Mutex::new(Vec::new()),
        disabled: AtomicBool::new(false),
        event_tx,
        event_drops: AtomicU64::new(0),
    });
    if has_event_hook {
        spawn_event_drain(Arc::downgrade(&conn), event_rx);
    }
    Ok(conn)
}

fn force_safer(declared: HookPosture, default: HookPosture) -> HookPosture {
    if declared == HookPosture::Safe || default == HookPosture::Safe {
        HookPosture::Safe
    } else {
        HookPosture::Open
    }
}

fn spawn_event_drain(conn: Weak<PluginConn>, mut rx: mpsc::Receiver<Envelope>) {
    tokio::spawn(async move {
        while let Some(envelope) = rx.recv().await {
            match conn.upgrade() {
                Some(conn) => conn.notify_event(envelope).await,
                None => break,
            }
        }
    });
}
