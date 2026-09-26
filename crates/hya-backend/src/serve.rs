use std::sync::Arc;

use anyhow::Context as _;
use hya_server::AppState;

use crate::cli_args::RelayFlags;
use crate::{db_lock, serve_relay};

use super::{
    agent_base_with_model, build_session_engine, build_session_engine_pure, open_store,
    resolve_runtime,
};

pub(crate) async fn cmd_serve(
    bind: String,
    db: String,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    relay: RelayFlags,
    allow_hosts: Vec<String>,
) -> anyhow::Result<()> {
    // A bad relay URL or CA file, or a bad --allow-host, fails before
    // anything starts.
    let relay_settings = serve_relay::settings(&relay)?;
    let hosts = crate::cli_args::serve_host_policy(&bind, &allow_hosts)?;
    // One writer per database: fail fast, before composing anything, when
    // another server owns it (ADR-0022).
    let lock = match db_lock::try_claim(&db).context("lock the database")? {
        db_lock::Claim::Owned(lock) => Some(lock),
        db_lock::Claim::Unlocked => None,
        db_lock::Claim::Busy(busy) => {
            eprintln!("{}", busy.serve_message());
            std::process::exit(db_lock::EXIT_DB_IN_USE);
        }
    };
    let prepared =
        prepare_server(&bind, db, lock, model_override, yolo, pure, &relay, &hosts).await?;
    // Join the relay before announcing readiness, so an identity-file
    // failure stops the start; the link is printed after the listen line.
    let link = match relay_settings {
        Some(settings) => Some(
            prepared
                .relay
                .connect(settings)
                .await
                .map_err(|error| anyhow::anyhow!("join the relay: {error}"))?,
        ),
        None => None,
    };
    // Install the termination handlers BEFORE announcing readiness. Callers that parse the
    // listen line and then signal us (the e2e harness) would otherwise race handler setup,
    // and losing that race means the default disposition kills the process outright —
    // measured: SIGTERM 7ms after the listen line died by signal, 500ms after it exited 0.
    let terminate = install_termination_signals().context("install termination handlers")?;
    println!("hya server listening on {}", prepared.url);
    if let Some(url) = &prepared.extra_url {
        println!("hya grpc listening on {url}");
    }
    emit_startup_mark("backend_listen", Some(&prepared.url));
    if let Some(link) = link
        && !relay.relay_quiet_link
    {
        serve_relay::print_link(&link.to_secret_string());
    }
    // Without a shutdown future `axum::serve` never returns, so `built.shutdown()` would be
    // unreachable and the process could only ever die by signal — skipping atexit handlers
    // (and therefore any coverage/profile flush). Handing it SIGTERM/Ctrl-C makes the
    // already-written teardown path run and lets `main` return normally.
    serve_until(prepared, wait_for_termination(terminate)).await
}

/// `hya serve start|status|stop|restart` (ADR-0023): control the backend
/// daemon of `db` (already resolved to the durable default when unset).
pub(crate) async fn cmd_serve_action(
    action: crate::cli_args::ServeAction,
    db: String,
    model: Option<String>,
    yolo: bool,
    pure: bool,
    parent_relay: RelayFlags,
    allow_hosts: Vec<String>,
) -> anyhow::Result<()> {
    use crate::cli_args::ServeAction;
    let allow_hosts = crate::cli_args::allow_host_names(&allow_hosts)?;
    // `hya serve --relay X start` means `hya serve start --relay X`.
    let relay_flags = |own: RelayFlags| -> anyhow::Result<RelayFlags> {
        let flags = if own.is_set() {
            own
        } else {
            parent_relay.clone()
        };
        let flags = serve_relay::absolutize(flags)?;
        serve_relay::settings(&flags)?;
        Ok(flags)
    };
    // After `start|restart --relay`: the daemon's link (its own output goes
    // to the log file, so it never prints it).
    let print_daemon_link = |ready: &daemon::Ready, relay: &RelayFlags| {
        let url = ready.discovery.url.clone();
        let asked = relay.relay.is_some();
        let started = ready.started;
        async move {
            if !asked {
                return;
            }
            if !started {
                eprintln!(
                    "hya: a server was already running; --relay was not applied (use `hya serve relay connect <url>`)"
                );
                return;
            }
            match serve_relay::call(&url, reqwest::Method::GET, "/v1/relay/link", None).await {
                Ok(value) => serve_relay::print_link(value["link"].as_str().unwrap_or("")),
                Err(error) => eprintln!("hya: could not read the relay link ({error:#})"),
            }
        }
    };
    use crate::daemon;
    let spec = |allow_hosts: Vec<String>| -> anyhow::Result<daemon::DaemonSpec> {
        Ok(daemon::DaemonSpec {
            db: db.clone(),
            model: model.clone(),
            yolo,
            pure,
            allow_hosts,
            exe: std::env::current_exe().context("find the hya binary")?,
        })
    };
    let print_ready = |ready: &daemon::Ready, json: bool| {
        if json {
            println!("{}", daemon::ready_json(ready, &db));
        } else {
            println!("{}", daemon::ready_line(ready, &db));
        }
        if let Some(note) = daemon::version_note(&ready.discovery) {
            eprintln!("{note}");
        }
    };
    // `quiet`: report on stderr (restart --json keeps stdout for the JSON).
    let stop = |timeout: u64, force: bool, quiet: bool, reason: hya_server::ShutdownReason| {
        let db = db.clone();
        async move {
            let stopped =
                daemon::stop(&db, std::time::Duration::from_secs(timeout), force, reason).await?;
            let line = match stopped {
                daemon::Stopped::NotRunning => format!("no hya server is running on {db}"),
                daemon::Stopped::Stopped { pid, killed } => {
                    let how = if killed { "killed" } else { "stopped" };
                    let mut line = format!("{how} hya server pid {pid} (db {db})");
                    if reason == hya_server::ShutdownReason::Stop {
                        // ADR-0023: after a manual stop, clients do not
                        // start the next server themselves.
                        line.push_str(
                            "\nconnected TUIs stay disconnected until /reconnect, or until a new hya client starts the next server",
                        );
                    }
                    line
                }
            };
            if quiet {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
            anyhow::Ok(())
        }
    };
    match action {
        ServeAction::Start { json, relay } => {
            let relay = relay_flags(relay)?;
            let asked_hosts = !allow_hosts.is_empty();
            let ready =
                daemon::start_with_relay(&spec(allow_hosts)?, &relay, daemon::START_WAIT).await?;
            print_ready(&ready, json);
            if asked_hosts && !ready.started {
                eprintln!(
                    "hya: a server was already running; --allow-host was not applied (use `hya serve restart --allow-host …`)"
                );
            }
            print_daemon_link(&ready, &relay).await;
        }
        ServeAction::Restart {
            json,
            force,
            timeout,
            relay,
        } => {
            // The new daemon rejoins the old one's relay (recorded in its
            // discovery file) unless told otherwise.
            let old = db_lock::holder(&db)
                .ok()
                .flatten()
                .and_then(|busy| busy.discovery);
            let relay = serve_relay::restart_flags(
                relay_flags(relay)?,
                old.as_ref().and_then(|found| found.relay.as_ref()),
            );
            // Likewise its --allow-host names, unless new ones are given.
            let allow_hosts = restart_allow_hosts(allow_hosts, old.as_ref());
            stop(timeout, force, json, hya_server::ShutdownReason::Restart).await?;
            let ready =
                daemon::start_with_relay(&spec(allow_hosts)?, &relay, daemon::START_WAIT).await?;
            print_ready(&ready, json);
            print_daemon_link(&ready, &relay).await;
        }
        ServeAction::Relay { action } => {
            serve_relay::run(action, &db).await?;
        }
        ServeAction::Stop { force, timeout } => {
            stop(timeout, force, false, hya_server::ShutdownReason::Stop).await?;
        }
        ServeAction::Status { json } => {
            let Some(found) = daemon::running(&db).await else {
                match db_lock::holder(&db).ok().flatten() {
                    Some(busy) => eprintln!(
                        "hya server pid {} holds {db} but does not answer (starting or stopping)",
                        busy.holder_pid
                            .map_or_else(|| "unknown".to_string(), |pid| pid.to_string())
                    ),
                    None => eprintln!("no hya server is running on {db}"),
                }
                std::process::exit(1);
            };
            let uptime = daemon::uptime(found.started_at);
            let log = daemon::log_path(&db).map(|path| path.to_string_lossy().into_owned());
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "url": found.url,
                        "pid": found.pid,
                        "version": found.version,
                        "startedAt": found.started_at,
                        "uptimeMs": u64::try_from(uptime.as_millis()).unwrap_or(u64::MAX),
                        "db": db,
                        "log": log,
                        "relay": found.relay,
                        "allowHosts": found.allow_hosts,
                    })
                );
            } else {
                println!("hya server pid {} running at {}", found.pid, found.url);
                println!("  version  {}", found.version);
                println!("  db       {db}");
                println!("  uptime   {}", daemon::human_duration(uptime));
                if let Some(log) = log {
                    println!("  log      {log}");
                }
                if let Some(relay) = &found.relay {
                    println!(
                        "  relay    {} (`hya serve relay status` for the connection)",
                        relay.proxy_url
                    );
                }
                if !found.allow_hosts.is_empty() {
                    println!("  hosts    {}", found.allow_hosts.join(", "));
                }
            }
            if let Some(note) = daemon::version_note(&found) {
                eprintln!("{note}");
            }
        }
    }
    Ok(())
}

/// The `--allow-host` names of the daemon `hya serve restart` starts: the
/// ones given to `restart`, else the old backend's (its discovery file).
pub(crate) fn restart_allow_hosts(
    explicit: Vec<String>,
    old: Option<&db_lock::Discovery>,
) -> Vec<String> {
    if !explicit.is_empty() {
        return explicit;
    }
    old.map(|found| found.allow_hosts.clone())
        .unwrap_or_default()
}

/// A composed `hya serve` whose listener is bound but not yet serving.
pub(crate) struct PreparedServer {
    /// `http://<addr>` of the bound listener (HTTP and gRPC).
    pub(crate) url: String,
    listener: tokio::net::TcpListener,
    /// `HYA_GRPC_BIND`: an optional extra listener serving the same server
    /// (and so the same state) as `listener`.
    extra: Option<tokio::net::TcpListener>,
    /// `http://<addr>` of `extra`.
    pub(crate) extra_url: Option<String>,
    /// The HTTP router and the gRPC services over one server state.
    server: hya_server::Server,
    built: hya_app::BuiltSessionEngine,
    /// The database lock; released (and the discovery file removed) only
    /// after the server has drained and shut down.
    lock: Option<db_lock::DbLock>,
    /// Ends every live event stream (SSE and gRPC) when the shutdown
    /// begins.
    streams: hya_server::StreamShutdown,
    /// The relay host connector (joined by `--relay` or `hya serve relay
    /// connect`); left after the drain.
    pub(crate) relay: hya_server::RelayHost,
}

/// Serve `prepared` until `stop` resolves, then drain and tear down.
///
/// On `stop`, drain first: every in-flight turn in every session (roots and
/// members) is cancelled with cause `shutdown` and closes its messages within
/// the drain deadline, members go terminal, and new turns are refused — so
/// open turn requests finish and the server can complete its graceful
/// shutdown. The spawn supervisor is shut down afterwards.
pub(crate) async fn serve_until(
    prepared: PreparedServer,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let PreparedServer {
        listener,
        extra,
        server,
        mut built,
        lock,
        streams,
        relay,
        ..
    } = prepared;
    let supervisor = built.resident_supervisor();
    let stop_request = lock.as_ref().map(db_lock::DbLock::stop_request_path);
    // The extra listener stops accepting when the shutdown begins, like the
    // main one.
    let stopping = tokio_util::sync::CancellationToken::new();
    let extra_served = extra.map(|listener| {
        let server = server.clone();
        let stopping = stopping.clone();
        tokio::spawn(async move {
            server
                .serve(listener, async move { stopping.cancelled().await })
                .await;
        })
    });
    // HTTP and gRPC on one listener; peer addresses let the loopback-only
    // rpcs (`RelayControl`) refuse non-loopback clients.
    server
        .serve(listener, async move {
            stop.await;
            stopping.cancel();
            // Why: `hya serve stop|restart` leave a request addressed to this
            // pid before their SIGTERM; anything else is a plain signal.
            let reason = stop_request
                .and_then(|path| db_lock::take_stop_request(&path, std::process::id()))
                .unwrap_or(hya_server::ShutdownReason::Signal);
            // End every client's live event stream (SSE and gRPC) first,
            // with that reason as the last frame: they never finish on their
            // own, and connected clients (the daemon outlives them,
            // ADR-0023) must not hold the shutdown open. Health answers
            // `unavailable` from here on.
            streams.close(reason);
            supervisor
                .drain(hya_proto::FinishCause::Shutdown, hya_core::DRAIN_DEADLINE)
                .await;
        })
        .await;
    if let Some(extra) = extra_served {
        let _ = extra.await;
    }
    // Relay streams already got their `serverStopping` frame and the drain
    // closed their turns: leave the relay (the room is released) and close
    // what is left of them.
    relay.shutdown().await;
    let shutdown_result = built.shutdown().await.context("shutdown spawn supervisor");
    // Last: remove the discovery file and release the lock.
    drop(lock);
    shutdown_result
}

/// Compose the runtime and bind `bind` (shared by `hya serve` and bare `hya`).
///
/// `lock` is the caller's claim on `db` ([`db_lock::try_claim`]); once the
/// listener is bound its discovery file is published.
// The composition inputs of one server; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_server(
    bind: &str,
    db: String,
    mut lock: Option<db_lock::DbLock>,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    relay_flags: &RelayFlags,
    hosts: &hya_server::HostPolicy,
) -> anyhow::Result<PreparedServer> {
    emit_startup_mark("backend_start", None);
    super::first_run_config_bootstrap(false)?;
    let store = open_store(&db).await?;
    emit_startup_mark("store_open", None);
    let mut runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    emit_startup_mark("runtime_resolved", None);
    let pending_discovery = std::mem::take(&mut runtime.pending_discovery);
    // Server AppState: base-only agent slot. Environment + AGENTS + references
    // are discovered per turn so Bundle Some does not drop project AGENTS and
    // Bundle None does not duplicate startup-baked AGENTS.
    let agent = Arc::new(agent_base_with_model(&runtime.model, runtime.reasoning));
    let mut built = if pure {
        build_session_engine_pure(
            store,
            runtime.router,
            agent.as_ref(),
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    } else {
        build_session_engine(
            store,
            runtime.router,
            agent.as_ref(),
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    };
    emit_startup_mark("engine_built", None);
    let engine = built.engine();
    let asks = built
        .take_asks()
        .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
    let questions = built
        .take_questions()
        .ok_or_else(|| anyhow::anyhow!("questions receiver missing"))?;
    let mcp_control = built.mcp_control();
    let agent_model_control = Arc::new(built.agent_model_control());
    let workflow_control = Arc::new(built.workflow_control());
    let plugin_host = built.plugin_host();
    let provider_manager = hya_app::ProviderManager::new(Arc::clone(&engine));
    let mut state = AppState::new(Arc::clone(&engine), agent)
        .with_provider_control(Arc::new(provider_manager.clone()))
        .with_question_requests(questions)
        .with_mcp_control(mcp_control)
        .with_workflow_control(workflow_control)
        .with_agent_model_control(agent_model_control)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone())
        .with_pure_guidance(pure)
        .with_auto_title(true)
        .with_allowed_hosts(hosts.clone());
    if yolo {
        eprintln!("hya: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
    }
    state = state.with_permission_requests(asks);
    // The relay host connector: its identity lives next to the database;
    // the discovery file records the relay it joins (never the link).
    let relay = hya_server::RelayHost::new(serve_relay::host_config(&db, relay_flags));
    if let Some(discovery) = lock.as_ref().map(db_lock::DbLock::discovery_path) {
        let heartbeat = relay_flags.relay_heartbeat;
        relay.set_settings_hook(Arc::new(move |settings| {
            let record = settings.map(|settings| serve_relay::discovered(settings, heartbeat));
            if let Err(error) = db_lock::set_discovery_relay(&discovery, record) {
                eprintln!("hya: could not record the relay in the discovery file ({error})");
            }
        }));
    }
    state = state.with_relay_host(relay.clone());
    // Remembered "allow always" grants survive restarts: reload them into
    // the process permission plane before serving.
    if let Err(error) = state.restore_saved_permissions().await {
        eprintln!("hya: failed to restore saved permission grants ({error})");
    }
    spawn_provider_catalog_refresh(
        provider_manager,
        state.catalog_updates_sender(),
        pending_discovery,
    );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr().context("read local addr")?;
    // Optional, legacy: HYA_GRPC_BIND=host:port serves the same server (HTTP
    // and gRPC, the same state) on an extra listener; the main listener
    // already answers gRPC.
    let extra = match std::env::var("HYA_GRPC_BIND")
        .ok()
        .filter(|value| !value.is_empty())
    {
        Some(extra_bind) => match tokio::net::TcpListener::bind(&extra_bind).await {
            Ok(extra) => Some(extra),
            Err(error) => {
                eprintln!("hya: HYA_GRPC_BIND={extra_bind}: could not listen ({error})");
                None
            }
        },
        None => None,
    };
    let extra_url = extra
        .as_ref()
        .and_then(|extra| extra.local_addr().ok())
        .map(|addr| format!("http://{addr}"));
    if let Some(lock) = lock.as_mut() {
        lock.publish_with(&db_lock::connect_url(addr), &hosts.extra_hosts())
            .context("publish the server discovery file")?;
    }
    // One server state for HTTP, gRPC, the extra listener, and the relay.
    let server = hya_server::build(state.clone());
    relay.set_service(server.clone());
    Ok(PreparedServer {
        url: format!("http://{addr}"),
        listener,
        extra,
        extra_url,
        streams: state.streams(),
        server,
        built,
        lock,
        relay,
    })
}

/// Background discovery for providers without cached remote models (and
/// discovery-only providers), serialized with live provider edits through the
/// provider manager's lock.
fn spawn_provider_catalog_refresh(
    manager: hya_app::ProviderManager,
    catalog_updates: tokio::sync::broadcast::Sender<serde_json::Value>,
    pending: Vec<hya_app::config::PendingCatalogDiscovery>,
) {
    if pending.is_empty() {
        return;
    }
    tokio::spawn(async move {
        match manager.refresh_pending(pending).await {
            Ok(false) => {}
            Ok(true) => {
                let payload = serde_json::json!({
                    "id": format!(
                        "catalog-{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|duration| duration.as_millis())
                            .unwrap_or(0)
                    ),
                    "type": "catalog.updated",
                    "properties": {}
                });
                let _ = catalog_updates.send(payload);
            }
            Err(error) => {
                eprintln!("hya: provider catalog refresh failed ({error:#})");
            }
        }
    });
}

/// Registered SIGTERM/SIGINT/SIGHUP streams, held so the handlers are live before we serve.
type TerminationSignals = (
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
);

/// Register the stop signals eagerly.
///
/// Returns the live streams; dropping them restores the default disposition, so the caller
/// must keep them until shutdown. Registration is eager so it wins the race against an
/// early SIGTERM (see the race note in `cmd_serve`).
/// SIGHUP is included because a terminal hangup should drain, not kill.
fn install_termination_signals() -> std::io::Result<TerminationSignals> {
    use tokio::signal::unix::{SignalKind, signal};
    Ok((
        signal(SignalKind::terminate())?,
        signal(SignalKind::interrupt())?,
        signal(SignalKind::hangup())?,
    ))
}

/// Resolve once any of the registered stop signals fires.
async fn wait_for_termination(signals: TerminationSignals) {
    let (mut terminate, mut interrupt, mut hangup) = signals;
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
        _ = hangup.recv() => {}
    }
}

/// Emit a structured startup mark when `HYA_STARTUP_TRACE` is truthy.
fn emit_startup_mark(mark: &str, detail: Option<&str>) {
    hya_app::startup_trace::mark(mark, detail);
}
