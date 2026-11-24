use std::sync::Arc;

use anyhow::Context as _;
use hya_server::AppState;

use crate::cli_args::RelayFlags;
use crate::{db_lock, serve_relay};

use hya_app::open_store;

pub(crate) async fn cmd_serve(
    bind: String,
    db: String,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    relay: RelayFlags,
    allow_hosts: Vec<String>,
) -> anyhow::Result<()> {
    let relay_settings = serve_relay::settings(&relay)?;
    let hosts = crate::cli_args::serve_host_policy(&bind, &allow_hosts)?;
    // Restored turns can run tools during bootstrap, so catch stop signals already.
    let terminate = install_termination_signals().context("install termination handlers")?;
    let prepared = match prepare_server(
        &bind,
        db.clone(),
        model_override,
        yolo,
        pure,
        &relay,
        &hosts,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            if runtime_owner_busy(&error) {
                if let Some(busy) = db_lock::holder(&db).ok().flatten() {
                    eprintln!("{}", busy.serve_message());
                } else {
                    eprintln!("hya: {error:#}");
                }
                std::process::exit(db_lock::EXIT_DB_IN_USE);
            }
            return Err(error);
        }
    };
    let link = match relay_settings {
        Some(settings) => match prepared.relay.connect(settings).await {
            Ok(link) => Some(link),
            Err(error) => {
                shutdown_prepared(prepared).await;
                return Err(error.into());
            }
        },
        None => None,
    };
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
    serve_until(prepared, terminate).await
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
            let old = daemon::running(&db).await;
            let relay = serve_relay::restart_flags(
                relay_flags(relay)?,
                old.as_ref().and_then(|found| found.relay.as_ref()),
            );
            let daemon_spec = spec(restart_allow_hosts(allow_hosts, old.as_ref()))?;
            stop(timeout, force, json, hya_server::ShutdownReason::Restart).await?;
            let ready = daemon::start_with_relay(&daemon_spec, &relay, daemon::START_WAIT).await?;
            if json {
                let mut value = daemon::ready_json(&ready, &db);
                value["restarted"] = serde_json::json!(true);
                println!("{value}");
            } else {
                print_ready(&ready, false);
            }
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
                match db_lock::holder(&db)? {
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
    /// Shared run registry, retained until shutdown has settled admitted turns.
    state: AppState,
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

/// Drain interrupted sessions before releasing the listeners and database locks.
pub(crate) async fn serve_until(
    prepared: PreparedServer,
    mut signals: TerminationSignals,
) -> anyhow::Result<()> {
    let PreparedServer {
        listener,
        extra,
        server,
        state,
        mut built,
        lock,
        streams,
        relay,
        ..
    } = prepared;
    let stop_request = lock.as_ref().map(db_lock::DbLock::stop_request_path);
    let drain_runtime = built.drain(hya_proto::FinishCause::Shutdown);
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
    let (drained, drain_result) = tokio::sync::oneshot::channel();
    server
        .serve(listener, async move {
            next_signal(&mut signals).await;
            let reason = stop_request
                .as_ref()
                .and_then(|path| db_lock::take_stop_request(path, std::process::id()))
                .map_or(hya_server::ShutdownReason::Signal, |notice| notice.reason);
            streams.close(reason);
            state.begin_shutdown();
            stopping.cancel();
            drain_runtime.await;
            let result = state.stop_for_restart(hya_engine::DRAIN_DEADLINE).await;
            let _ = drained.send(result);
        })
        .await;
    let extra_result = match extra_served {
        Some(task) => task.await.context("join extra gRPC listener"),
        None => Ok(()),
    };
    relay.shutdown().await;
    drop(lock);
    let shutdown_result = built.shutdown().await.context("shutdown spawn supervisor");
    drop(built);
    drain_result.await.context("receive drain result")??;
    extra_result?;
    shutdown_result
}

/// Tear down a [`PreparedServer`] through the same shutdown path as a normal stop
/// when post-prepare startup (such as relay connect) fails.
async fn shutdown_prepared(prepared: PreparedServer) {
    let PreparedServer {
        mut built,
        lock,
        relay,
        ..
    } = prepared;
    relay.shutdown().await;
    drop(lock);
    if let Err(error) = built.shutdown().await {
        eprintln!("hya: shutdown after startup failure failed: {error:#}");
    }
}

/// Whether startup failed because another process still holds the runtime
/// owner of the database (`RUNTIME_OWNER_BUSY`), e.g. a server shutting down.
fn runtime_owner_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<hya_store::StoreError>(),
            Some(hya_store::StoreError::RuntimeOwnerBusy)
        )
    })
}

/// Compose the runtime, bind fresh listeners, and publish discovery.
// The composition inputs of one server; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_server(
    bind: &str,
    db: String,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    relay_flags: &RelayFlags,
    hosts: &hya_server::HostPolicy,
) -> anyhow::Result<PreparedServer> {
    emit_startup_mark("backend_start", None);
    let store = open_store(&db).await?;
    let exit_store = store.clone();
    match prepare_server_with_store(
        store,
        bind,
        &db,
        model_override,
        yolo,
        pure,
        relay_flags,
        hosts,
    )
    .await
    {
        Ok(prepared) => Ok(prepared),
        Err(error) => {
            exit_store
                .drain_and_report_exit(hya_store::EXIT_DRAIN_TIMEOUT)
                .await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_server_with_store(
    store: hya_store::SessionStore,
    bind: &str,
    db: &str,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    relay_flags: &RelayFlags,
    hosts: &hya_server::HostPolicy,
) -> anyhow::Result<PreparedServer> {
    let has_file_store = store.paths().is_some();
    emit_startup_mark("store_open", None);
    crate::first_run_config_bootstrap(false)?;
    let config = hya_app::resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    emit_startup_mark("runtime_resolved", None);
    let engine = hya_app::ServerEngine::build(store, config).await?;
    let mut lock = if has_file_store {
        db_lock::owned_metadata(db)?
    } else {
        None
    };
    emit_startup_mark("engine_built", None);
    let runtime = engine.assemble(hosts.clone())?;
    if yolo {
        eprintln!("hya: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
    }
    // The relay host connector: its identity lives next to the database;
    // the discovery file records the relay it joins (never the link).
    let relay = hya_server::RelayHost::new(serve_relay::host_config(db, relay_flags));
    if let Some(discovery) = lock.as_ref().map(db_lock::DbLock::discovery_path) {
        let heartbeat = relay_flags.relay_heartbeat;
        relay.set_settings_hook(Arc::new(move |settings| {
            let record = settings.map(|settings| serve_relay::discovered(settings, heartbeat));
            if let Err(error) = db_lock::set_discovery_relay(&discovery, record) {
                eprintln!("hya: could not record the relay in the discovery file ({error})");
            }
        }));
    }
    let mut runtime = runtime.with_relay_host(relay.clone());
    // Remembered grants must be loaded before any restore runs tools.
    if let Err(error) = runtime.app_state().restore_saved_permissions().await {
        eprintln!("hya: failed to restore saved permission grants ({error})");
    }
    runtime.spawn_provider_catalog_refresh(|error| {
        eprintln!("hya: provider catalog refresh failed ({error:#})");
    });
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context("bind server listener")?;
    let addr = listener.local_addr().context("read local addr")?;
    let extra = match std::env::var("HYA_GRPC_BIND")
        .ok()
        .filter(|value| !value.is_empty())
    {
        Some(extra_bind) => Some(
            tokio::net::TcpListener::bind(&extra_bind)
                .await
                .with_context(|| format!("bind HYA_GRPC_BIND={extra_bind}"))?,
        ),
        None => None,
    };
    let extra_url = extra
        .as_ref()
        .and_then(|extra| extra.local_addr().ok())
        .map(|addr| format!("http://{addr}"));
    // Build the complete shared server before advertising readiness.
    let server = hya_server::build(runtime.app_state().clone());
    relay.set_service(server.clone());
    let resumed = runtime
        .resume_interrupted()
        .await
        .context("admit restores and start recovered residents")?;
    if !resumed.is_empty() {
        eprintln!(
            "hya: restored {} interrupted session turn(s)",
            resumed.len()
        );
    }
    if let Some(lock) = lock.as_mut() {
        lock.publish_with(&db_lock::connect_url(addr), &hosts.extra_hosts())
            .context("publish the server discovery file")?;
    }
    let (built, state) = runtime.into_parts();
    Ok(PreparedServer {
        url: format!("http://{addr}"),
        listener,
        extra,
        extra_url,
        streams: state.streams(),
        state,
        server,
        built,
        lock,
        relay,
    })
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

/// Resolve when any registered termination signal fires.
async fn next_signal(signals: &mut TerminationSignals) {
    let (terminate, interrupt, hangup) = signals;
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod runtime_owner_busy_tests {
    use super::{PreparedServer, prepare_server, runtime_owner_busy, shutdown_prepared};
    use crate::cli_args::RelayFlags;

    #[test]
    fn a_held_runtime_owner_anywhere_in_the_chain_is_busy() {
        let busy = anyhow::Error::new(hya_store::StoreError::RuntimeOwnerBusy)
            .context("claim runtime owner before startup recovery");
        assert!(runtime_owner_busy(&busy));
        let other = anyhow::anyhow!("bind 127.0.0.1:0").context("listen");
        assert!(!runtime_owner_busy(&other));
    }

    #[tokio::test]
    async fn shutdown_prepared_drains_store_after_post_prepare_failure() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hya-serve-post-prepare-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("s.db");
        let hosts = hya_server::HostPolicy::loopback();
        let prepared: PreparedServer = prepare_server(
            "127.0.0.1:0",
            db.to_string_lossy().into_owned(),
            Some("hya/offline".into()),
            false,
            true,
            &RelayFlags::default(),
            &hosts,
        )
        .await
        .unwrap();
        let store = prepared.built.engine().store().clone();
        // Force degradation before post-prepare teardown (e.g. relay failure) runs.
        std::fs::remove_file(&db).unwrap();
        let session = hya_proto::SessionId::new();
        store
            .append_event(
                session,
                &hya_proto::Event::SessionTitled {
                    session,
                    title: "unpersisted-on-relay-fail".into(),
                },
            )
            .await
            .unwrap();
        shutdown_prepared(prepared).await;
        let mut line = Vec::new();
        let report = store
            .drain_and_report_exit_to(std::time::Duration::from_secs(1), &mut line)
            .await;
        assert!(
            matches!(
                report,
                hya_store::ExitDrainReport::Unpersisted { events: 1, .. }
            ),
            "expected Unpersisted report after shutdown_prepared, got {report:?}"
        );
        // BuiltSessionEngine::shutdown already emitted the once-per-store stderr line.
        assert!(
            line.is_empty(),
            "shutdown_prepared should already have consumed exit report: {}",
            String::from_utf8_lossy(&line)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
