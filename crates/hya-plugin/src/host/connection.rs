use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use hya_proto::Envelope;
use tokio::sync::mpsc;

use crate::client::{DEFAULT_CALL_TIMEOUT, PluginClient};
use crate::config::PluginSpec;
use crate::error::PluginError;
use crate::messages::{HookName, HookPosture, HostInfo, PROTOCOL_VERSION};

use super::{EVENT_CHANNEL_CAP, LiveClient, PluginConn};

pub(super) async fn connect_one(
    spec: PluginSpec,
    host: HostInfo,
) -> Result<Arc<PluginConn>, PluginError> {
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
        workspace_adapters: init.workspace_adapters,
        timeout,
        command: spec.command,
        env: spec.env,
        host_info: host,
        live: tokio::sync::Mutex::new(Some(LiveClient {
            client,
            _guard: guard,
        })),
        restarts: tokio::sync::Mutex::new(Vec::new()),
        disabled: std::sync::atomic::AtomicBool::new(false),
        event_tx,
        event_drops: std::sync::atomic::AtomicU64::new(0),
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
