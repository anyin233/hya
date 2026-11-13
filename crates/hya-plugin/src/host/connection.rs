use std::collections::HashSet;
use std::sync::{Arc, Weak};
use std::time::Duration;

use hya_proto::Envelope;
use tokio::sync::mpsc;

use crate::client::{DEFAULT_CALL_TIMEOUT, PluginClient};
use crate::config::{PluginSpec, ProcessLaunch};
use crate::error::PluginError;
use crate::messages::{HookName, HostInfo, InitializeResult};

use super::{EVENT_CHANNEL_CAP, LiveClient, PluginConn, canonical_initialize, validate_initialize};

pub(super) async fn connect_one(
    spec: PluginSpec,
    host: HostInfo,
    host_reads: Option<Arc<dyn hya_core::HostSessionReads>>,
) -> Result<Arc<PluginConn>, PluginError> {
    let timeout = spec
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CALL_TIMEOUT);
    let (live, init) = start_client(&spec, &host).await?;
    let canonical_declaration = Arc::<[u8]>::from(canonical_initialize(&init)?);
    let contributions = init.contributions.clone();
    let hooks = contributions
        .hooks
        .iter()
        .map(|hook| hook.name)
        .collect::<HashSet<_>>();
    let has_event_hook = hooks.contains(&HookName::Event);
    // Only bundle processes receive capabilities, so only they keep host reads.
    let host_reads = spec.has_capabilities().then_some(host_reads).flatten();
    let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAP);
    let activation_loss = spec.is_activation().then(|| live.client.closed_token());
    let conn = Arc::new(PluginConn {
        io_tasks: hya_mcp::io_tasks::current(),
        id: spec.id.clone(),
        hooks,
        contributions,
        canonical_declaration,
        timeout,
        spec,
        host_info: host,
        live: std::sync::Mutex::new(Some(live)),
        connecting: tokio::sync::Mutex::new(()),
        restarts: tokio::sync::Mutex::new(Vec::new()),
        disabled: std::sync::atomic::AtomicBool::new(false),
        stopped: std::sync::atomic::AtomicBool::new(false),
        activation_loss,
        declaration_drift: std::sync::atomic::AtomicBool::new(false),
        event_tx,
        event_drops: std::sync::atomic::AtomicU64::new(0),
        host_reads,
        event_task: std::sync::Mutex::new(None),
    });
    if has_event_hook {
        let task = spawn_event_drain(Arc::downgrade(&conn), event_rx);
        *conn
            .event_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }
    Ok(conn)
}

/// Initial connection and supervised reconnection use the same launch and handshake.
pub(super) async fn start_client(
    spec: &PluginSpec,
    host: &HostInfo,
) -> Result<(LiveClient, InitializeResult), PluginError> {
    let env = (!spec.env.is_empty()).then_some(&spec.env);
    let (client, mut guard) = match &spec.launch {
        ProcessLaunch::Config => PluginClient::spawn(&spec.command, env)?,
        ProcessLaunch::Project(root) => PluginClient::spawn_in(&spec.command, root, env)?,
        ProcessLaunch::Bundle { root, .. } => PluginClient::spawn_bundle(&spec.command, root, env)?,
    };
    let initialized = match &spec.launch {
        ProcessLaunch::Bundle {
            activation: Some(metadata),
            ..
        } => {
            client
                .initialize_activation(host.clone(), metadata.clone())
                .await
        }
        _ => client.initialize(host.clone()).await,
    };
    let initialized = initialized.and_then(|init| {
        validate_initialize(spec, &init)?;
        Ok(init)
    });
    match initialized {
        Ok(init) => Ok((LiveClient { client, guard }, init)),
        Err(error) => {
            let _ = guard.terminate().await;
            Err(error)
        }
    }
}

fn spawn_event_drain(
    conn: Weak<PluginConn>,
    mut rx: mpsc::Receiver<Envelope>,
) -> tokio::task::JoinHandle<()> {
    hya_mcp::io_tasks::spawn(async move {
        while let Some(envelope) = rx.recv().await {
            let client = conn
                .upgrade()
                .and_then(|conn| conn.lock_live().as_ref().map(|live| live.client.clone()));
            match client {
                Some(client) => PluginConn::notify_event(client, envelope).await,
                None => break,
            }
        }
    })
}
