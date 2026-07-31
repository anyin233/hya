//! `PluginHost` — spawns every configured plugin, preserves declared load
//! order, runs the hook chains the bridges drive, and supervises each child:
//! a crash (EOF mid-call) marks the plugin `Dead`, the next call respawns it,
//! and exceeding the restart budget moves it to `Disabled`. Generalizes
//! `hya_mcp::manager::McpManager`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hya_proto::{Envelope, SessionId, ToolCallId, WorkspaceAdapterInfo};
use hya_tool::Tool;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

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
    DeclarationDrift,
    Disabled,
}

pub struct PluginHost {
    plugins: Vec<Arc<PluginConn>>,
}

#[derive(Clone)]
pub struct PreparedPlugin {
    conn: Arc<PluginConn>,
}

impl PreparedPlugin {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.conn.id
    }

    #[must_use]
    pub fn canonical_declaration(&self) -> &[u8] {
        &self.conn.canonical_declaration
    }

    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.conn
            .tools
            .iter()
            .filter_map(|tool| PluginTool::try_new(self.conn.clone(), tool.clone()))
            .collect()
    }
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
    canonical_declaration: Arc<[u8]>,
    pub(crate) timeout: Duration,
    command: Vec<String>,
    env: BTreeMap<String, String>,
    host_info: HostInfo,
    live: Mutex<Option<LiveClient>>,
    restarts: Mutex<Vec<Instant>>,
    disabled: AtomicBool,
    declaration_drift: AtomicBool,
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
        } else if self.declaration_drift.load(Ordering::Relaxed) {
            PluginStatus::DeclarationDrift
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
        if self.declaration_drift.load(Ordering::Relaxed) {
            return Err(PluginError::DeclarationDrift {
                plugin: self.id.clone(),
            });
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
        let init = client.initialize(self.host_info.clone()).await?;
        validate_initialize(&self.id, &init)?;
        if canonical_initialize(&init)?.as_slice() != self.canonical_declaration.as_ref() {
            self.declaration_drift.store(true, Ordering::Relaxed);
            return Err(PluginError::DeclarationDrift {
                plugin: self.id.clone(),
            });
        }
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

fn validate_initialize(
    configured_id: &str,
    init: &crate::messages::InitializeResult,
) -> Result<(), PluginError> {
    if init.protocol_version != crate::messages::PROTOCOL_VERSION {
        return Err(PluginError::ProtocolMismatch {
            expected: crate::messages::PROTOCOL_VERSION,
            got: init.protocol_version,
        });
    }
    if init.plugin.id != configured_id {
        return Err(PluginError::IdentityMismatch {
            expected: configured_id.to_string(),
            got: init.plugin.id.clone(),
        });
    }
    Ok(())
}

fn canonical_initialize(init: &crate::messages::InitializeResult) -> Result<Vec<u8>, PluginError> {
    let mut declaration = init.clone();
    declaration.hooks.sort_by_key(|hook| {
        (
            hook.name,
            hook.posture.map_or("", |posture| match posture {
                HookPosture::Safe => "safe",
                HookPosture::Open => "open",
            }),
        )
    });
    declaration.tools.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.description.cmp(&right.description))
            .then_with(|| {
                canonical_json(&left.input_schema)
                    .to_string()
                    .cmp(&canonical_json(&right.input_schema).to_string())
            })
    });
    declaration.workspace_adapters.sort_by(|left, right| {
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
    pub async fn connect_all(specs: Vec<PluginSpec>, host: HostInfo) -> Self {
        Self::connect_all_observed(specs, host).await.0
    }

    pub async fn connect_all_observed(
        specs: Vec<PluginSpec>,
        host: HostInfo,
    ) -> (Self, BTreeMap<String, PluginError>) {
        let mut set = tokio::task::JoinSet::new();
        for (index, spec) in specs.into_iter().enumerate() {
            let host = host.clone();
            let id = spec.id.clone();
            set.spawn(async move { (index, id, connection::connect_one(spec, host).await) });
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
    pub fn prepared_plugins(&self) -> Vec<PreparedPlugin> {
        self.plugins
            .iter()
            .map(|conn| PreparedPlugin { conn: conn.clone() })
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

#[cfg(test)]
mod tests {
    use hya_proto::WorkspaceAdapterInfo;
    use serde_json::json;

    use super::canonical_initialize;
    use crate::messages::{
        HookName, HookPosture, HookRegistration, InitializeResult, PROTOCOL_VERSION, PluginInfo,
        PluginKindWire, ToolInfo,
    };

    fn declaration() -> InitializeResult {
        InitializeResult {
            protocol_version: PROTOCOL_VERSION,
            plugin: PluginInfo {
                id: "complete-declaration".to_string(),
                version: "1.0.0".to_string(),
                kind: PluginKindWire::Rust,
            },
            hooks: vec![
                HookRegistration {
                    name: HookName::PermissionAsk,
                    posture: Some(HookPosture::Safe),
                },
                HookRegistration {
                    name: HookName::CommandExecuteBefore,
                    posture: Some(HookPosture::Open),
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
        }
    }

    #[test]
    fn initialize_declaration_is_order_independent_and_complete() {
        let original = declaration();
        let mut reordered = original.clone();
        reordered.hooks.reverse();
        reordered.tools.reverse();
        reordered.workspace_adapters.reverse();
        reordered.tools[1].input_schema = json!({
            "type": "object",
            "properties": { "first": { "type": "boolean" }, "second": { "type": "string" } }
        });

        let canonical = canonical_initialize(&original).unwrap_or_default();
        assert_eq!(
            canonical,
            canonical_initialize(&reordered).unwrap_or_default()
        );

        let mut changed_hook = original.clone();
        changed_hook.hooks[0].posture = Some(HookPosture::Open);
        assert_ne!(
            canonical,
            canonical_initialize(&changed_hook).unwrap_or_default()
        );

        let mut changed_tool = original.clone();
        changed_tool.tools[0].description = "changed".to_string();
        assert_ne!(
            canonical,
            canonical_initialize(&changed_tool).unwrap_or_default()
        );

        let mut changed_adapter = original;
        changed_adapter.workspace_adapters[0].description = "changed".to_string();
        assert_ne!(
            canonical,
            canonical_initialize(&changed_adapter).unwrap_or_default()
        );
    }
}
