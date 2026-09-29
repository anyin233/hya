#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use hya_proto::{Event, MessageId, PartId, ToolCallId};
use hya_server::AppState;

use crate::cli_args::RelayFlags;
use crate::{daemon, db_lock, serve_relay};

use super::{
    agent_base_with_model, build_session_engine, build_session_engine_pure, open_store,
    resolve_runtime,
};

/// The inherited state of a restart successor, captured before the Tokio
/// runtime exists: the held lock descriptor, the journal, the optional
/// extra gRPC listener, and the old generation's status timestamp
/// (`--inherit-status`). The main listener flows through the regular
/// `--listen-fd` capture.
pub(crate) struct InheritedLock {
    pub(crate) file: std::fs::File,
    pub(crate) journal: PathBuf,
    pub(crate) started_at: Option<u64>,
    pub(crate) extra: Option<std::net::TcpListener>,
}

/// Longest wait for the old generation to record `released` (its runtime is
/// gone and the store may be opened). The old generation releases right
/// after spawning the successor, so this only bounds a stuck predecessor.
const HANDOFF_RELEASED_WAIT: Duration = Duration::from_secs(60);

/// Longest wait for the handoff resume driver (`run_handoff_resume`); the
/// successor refuses to serve half-resumed.
const HANDOFF_RESUME_BOUND: Duration = Duration::from_secs(30);
const SUCCESSOR_READY_WAIT: Duration = Duration::from_secs(90);

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_serve(
    bind: String,
    inherited: Option<std::net::TcpListener>,
    inherited_lock: Option<InheritedLock>,
    db: String,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    relay: RelayFlags,
    allow_hosts: Vec<String>,
) -> anyhow::Result<()> {
    // A bad relay URL or CA file, or a bad --allow-host, fails before
    // anything starts.
    if inherited.is_none() && inherited_lock.is_some() {
        anyhow::bail!(
            "a handoff successor needs the listener descriptor too (--listen-fd with --lock-fd)"
        );
    }
    let inherited_journal_for_error = inherited_lock.as_ref().map(|lock| lock.journal.clone());
    let record_inherited_failure = |message: String| {
        if let Some(journal) = &inherited_journal_for_error {
            let _ = db_lock::write_handoff_stage(
                journal,
                db_lock::HandoffStage::Failed,
                std::process::id(),
                None,
                Some(&message),
            );
        }
        anyhow::anyhow!(message)
    };
    let relay_settings = serve_relay::settings(&relay)
        .map_err(|error| record_inherited_failure(format!("resolve relay settings: {error:#}")))?;
    let hosts = crate::cli_args::serve_host_policy(&bind, &allow_hosts)
        .map_err(|error| record_inherited_failure(format!("resolve host policy: {error:#}")))?;
    // A handoff successor waits for the old generation's `released` stage
    // before it touches the store: the release is the one-writer handover,
    // and the journal names the generation that actually held the lock.
    let mut released = None;
    let mut successor_journal: Option<PathBuf> = None;
    let mut successor_extra: Option<std::net::TcpListener> = None;
    // A restart successor adopts the already-held flock; normal foreground
    // starts retain the fail-fast one-writer claim.
    let lock = match inherited_lock {
        Some(inherited) => {
            let state = db_lock::wait_handoff(
                &inherited.journal,
                db_lock::HandoffStage::Released,
                HANDOFF_RELEASED_WAIT,
            )
            .await
            .map_err(|error| {
                record_inherited_failure(format!(
                    "wait for the old generation to release {db}: {error:#}"
                ))
            })?;
            if state.stage() == db_lock::HandoffStage::Failed {
                anyhow::bail!(
                    "the handoff failed in the old generation: {}",
                    state.error.as_deref().unwrap_or("no reason given")
                );
            }
            let lock = db_lock::adopt(&db, inherited.file, inherited.started_at)
                .context("adopt the database lock")?;
            if let (Some(released_pid), Some(predecessor)) =
                (state.released_pid(), lock.predecessor_pid())
                && released_pid != predecessor
            {
                anyhow::bail!(
                    "the handoff journal names predecessor pid {released_pid}, but the \
                     inherited lock was held by pid {predecessor}"
                );
            }
            released = Some(state);
            successor_journal = Some(inherited.journal);
            successor_extra = inherited.extra;
            Some(lock)
        }
        None => match db_lock::try_claim(&db).context("lock the database")? {
            db_lock::Claim::Owned(lock) => Some(lock),
            db_lock::Claim::Unlocked => None,
            db_lock::Claim::Busy(busy) => {
                eprintln!("{}", busy.serve_message());
                std::process::exit(db_lock::EXIT_DB_IN_USE);
            }
        },
    };
    let exe = std::env::current_exe().context("find the hya binary")?;
    let handoff_spec = db_lock::paths(&db).map(|_| daemon::DaemonSpec {
        db: db.clone(),
        model: model_override.clone(),
        yolo,
        pure,
        allow_hosts: allow_hosts.clone(),
        exe,
    });
    let inherited_extra = successor_extra.take();
    let resume_fatal = successor_journal.is_some();
    let prepared = match prepare_server(
        &bind,
        inherited,
        db.clone(),
        lock,
        model_override,
        yolo,
        pure,
        &relay,
        &hosts,
        handoff_spec,
        relay.clone(),
        inherited_extra,
        resume_fatal,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            // A failed successor start is recorded, never silent: the old
            // generation sees the journal and keeps the ownership.
            if let Some(journal) = &successor_journal {
                let _ = db_lock::write_handoff_stage(
                    journal,
                    db_lock::HandoffStage::Failed,
                    std::process::id(),
                    None,
                    Some(&format!("{error:#}")),
                );
            } else if runtime_owner_busy(&error) {
                // The previous server released the database lock but is
                // still releasing its runtime owner: the database is in use,
                // so a starter waits and tries again (`daemon::start`).
                eprintln!("hya: {error:#}");
                std::process::exit(db_lock::EXIT_DB_IN_USE);
            }
            return Err(error);
        }
    };
    // Join the relay before announcing readiness, so an identity-file
    // failure stops the start; the link is printed after the listen line.
    let link = match relay_settings {
        Some(settings) => match prepared.relay.connect(settings).await {
            Ok(link) => Some(link),
            Err(error) => {
                let failure = record_inherited_failure(format!("join the relay: {error}"));
                return Err(failure);
            }
        },
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
    // The successor's discovery is published and its resume is done: record
    // `ready`, then watch for the predecessor's exit to complete the
    // transfer (`transferred`).
    if let (Some(journal), Some(state)) = (&successor_journal, released.take())
        && let Some(predecessor) = state.released_pid()
    {
        db_lock::write_handoff_stage(
            journal,
            db_lock::HandoffStage::Ready,
            std::process::id(),
            None,
            None,
        )
        .context("record the handoff ready stage")?;
        let journal = journal.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
            while daemon::process_alive(predecessor) {
                if tokio::time::Instant::now() >= deadline {
                    eprintln!(
                        "hya: the old generation (pid {predecessor}) has not exited; the handoff transfer stays open"
                    );
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let _ = db_lock::write_handoff_stage(
                &journal,
                db_lock::HandoffStage::Transferred,
                std::process::id(),
                None,
                None,
            );
        });
    }
    // Without a shutdown future `axum::serve` never returns, so `built.shutdown()` would be
    // unreachable and the process could only ever die by signal — skipping atexit handlers
    // (and therefore any coverage/profile flush). Handing it SIGTERM/Ctrl-C makes the
    // already-written teardown path run and lets `main` return normally.
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
        ServeAction::Check { json } => {
            crate::self_check::cmd_check(&db, model.clone(), pure, json).await;
        }
        ServeAction::Restart {
            json,
            force,
            timeout,
            relay,
            verify,
            exe,
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
            let mut daemon_spec = spec(allow_hosts)?;
            if let Some(exe) = exe {
                daemon_spec.exe = std::path::absolute(&exe)
                    .with_context(|| format!("resolve --exe {}", exe.display()))?;
                daemon::validate_successor_exe(&daemon_spec.exe)?;
            }
            // Self-proof first: the successor build must compose its runtime
            // against this database and pass every --verify command before
            // the running daemon is asked for anything.
            let check = crate::self_check::restart_gate(
                &daemon_spec.exe,
                &db,
                daemon_spec.model.as_deref(),
                pure,
                &verify,
            )
            .await?;
            if !json {
                eprintln!(
                    "self-check passed: {} serve check{}",
                    daemon_spec.exe.display(),
                    if verify.is_empty() {
                        String::new()
                    } else {
                        format!(", {} verify command(s)", verify.len())
                    }
                );
            }
            match daemon::restart_by_handoff(&db, &daemon_spec, &relay).await? {
                daemon::HandoffRestart::Queued(queued) => {
                    // The old generation owns the rest of the handoff (it
                    // rolls back to its pinned generation if the successor
                    // fails). The successor keeps this URL, so this return
                    // value stays accurate; its pid is `hya serve status`'s.
                    if json {
                        let mut value = daemon::queued_json(&queued, &db);
                        value["check"] = check;
                        println!("{value}");
                    } else {
                        println!("{}", daemon::queued_line(&queued, &db));
                    }
                }
                daemon::HandoffRestart::NotApplicable => {
                    let ready = {
                        stop(timeout, force, json, hya_server::ShutdownReason::Restart).await?;
                        daemon::start_with_relay(&daemon_spec, &relay, daemon::START_WAIT).await?
                    };
                    print_ready(&ready, json);
                    print_daemon_link(&ready, &relay).await;
                }
            }
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
            // A restart that failed and rolled back to the previous build
            // is reported until the next restart replaces the journal.
            let rolled_back = db_lock::paths(&db)
                .and_then(|paths| db_lock::read_handoff(&paths.handoff))
                .filter(|state| state.stage_pid(db_lock::HandoffStage::Ready) == Some(found.pid))
                .and_then(|state| state.spec.and_then(|spec| spec.rolled_back_from));
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
                        "lastRestart": rolled_back.as_ref().map(|error| serde_json::json!({
                            "rolledBack": true,
                            "error": error,
                        })),
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
                if let Some(error) = &rolled_back {
                    println!("  restart  failed and rolled back to the previous build: {error}");
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

async fn listener_from_inherited_or_bind(
    inherited: Option<std::net::TcpListener>,
    bind: &str,
) -> anyhow::Result<tokio::net::TcpListener> {
    match inherited {
        // Already validated, close-on-exec, and nonblocking (captured before
        // the runtime existed); Tokio adoption is the single ownership
        // transfer.
        Some(listener) => tokio::net::TcpListener::from_std(listener)
            .context("adopt inherited listener into Tokio"),
        None => tokio::net::TcpListener::bind(bind)
            .await
            .with_context(|| format!("bind {bind}")),
    }
}

/// Capture the inherited state of a handoff successor before Tokio can
/// reuse descriptor numbers: the `<db>.lock` descriptor, the journal path,
/// the inherited status timestamp, and the optional extra gRPC listener.
/// The main listener is captured by [`capture_cli_listener`] (`--listen-fd`).
pub(crate) fn capture_cli_lock(
    cli: &crate::cli_args::Cli,
) -> anyhow::Result<Option<InheritedLock>> {
    let (lock_fd, journal, started_at, extra_fd) = match &cli.command {
        Some(crate::cli_args::Command::Serve {
            lock_fd: Some(fd),
            handoff_journal: Some(journal),
            inherit_status,
            grpc_listen_fd,
            action: None,
            ..
        }) => (*fd, journal.clone(), *inherit_status, *grpc_listen_fd),
        _ => return Ok(None),
    };
    let extra = match extra_fd {
        Some(fd) => Some(inherited_std_listener(fd)?),
        None => None,
    };
    Ok(Some(InheritedLock {
        file: inherited_lock_file(lock_fd)?,
        journal,
        started_at,
        extra,
    }))
}

#[cfg(unix)]
fn inherited_lock_file(fd: u32) -> anyhow::Result<std::fs::File> {
    let fd = i32::try_from(fd).context("inherited lock FD is too large")?;
    if fd < 3 {
        anyhow::bail!("inherited lock FD must be >= 3, got {fd}");
    }
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err(std::io::Error::last_os_error()).context("inherited lock FD is not open");
    }
    // SAFETY: the descriptor is validated open and ownership is transferred
    // exactly once to the returned File.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error())
            .context("inspect inherited lock close-on-exec flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("set inherited lock close-on-exec flag");
    }
    Ok(file)
}

#[cfg(not(unix))]
fn inherited_lock_file(_fd: u32) -> anyhow::Result<std::fs::File> {
    anyhow::bail!("--lock-fd is only supported on Unix");
}
/// Capture the `--listen-fd` supervisor listener synchronously, before the
/// Tokio runtime exists (called from [`crate::main`]).
///
/// Returns `None` unless this is a foreground `hya serve --listen-fd`; the
/// daemon-action rejection stays with the CLI dispatch. On success the
/// descriptor is an owned std listener, already validated, marked
/// close-on-exec, and nonblocking; [`prepare_server`] converts it to the
/// async listener exactly once.
pub(crate) fn capture_cli_listener(
    cli: &crate::cli_args::Cli,
) -> anyhow::Result<Option<std::net::TcpListener>> {
    let listen_fd = match &cli.command {
        Some(crate::cli_args::Command::Serve {
            listen_fd: Some(fd),
            action: None,
            ..
        }) => *fd,
        _ => return Ok(None),
    };
    inherited_std_listener(listen_fd).map(Some)
}

#[cfg(unix)]
fn inherited_std_listener(fd: u32) -> anyhow::Result<std::net::TcpListener> {
    let fd = i32::try_from(fd).context("inherited listener FD is too large")?;
    if fd < 3 {
        anyhow::bail!("inherited listener FD must be >= 3, got {fd}");
    }
    // Validate without owning the descriptor: a rejected FD is never adopted
    // and never closed by hya, so an invalid or reused number is a normal
    // error instead of an IO-safety abort when dropping a bad owner.
    validate_inherited_listener_fd(fd)?;
    // SAFETY: ownership is taken exactly once, after the descriptor was
    // confirmed open and an IPv4/IPv6 TCP listener; every error below drops
    // the std listener, closing the FD owned here.
    let listener = unsafe { std::net::TcpListener::from_raw_fd(fd as RawFd) };
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error())
            .context("inspect inherited listener close-on-exec flags");
    }
    // Close-on-exec is set before composition: processes spawned later
    // (bundles, tools, restart children) must not inherit the listener.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("set inherited listener close-on-exec flag");
    }
    listener
        .set_nonblocking(true)
        .context("set inherited listener nonblocking")?;
    Ok(listener)
}

#[cfg(not(unix))]
fn inherited_std_listener(_fd: u32) -> anyhow::Result<std::net::TcpListener> {
    anyhow::bail!("--listen-fd is only supported on Unix");
}
/// Darwin does not implement `SO_ACCEPTCONN` for TCP sockets. Its TCP state
/// structure exposes the same kernel state without consuming a pending client.
#[cfg(all(unix, target_vendor = "apple"))]
fn inherited_listener_is_listening(fd: RawFd) -> anyhow::Result<bool> {
    let mut info: libc::tcp_connection_info = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::tcp_connection_info>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_CONNECTION_INFO,
            std::ptr::from_mut(&mut info).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("read the inherited listener TCP state");
    }
    // Darwin's `TCPS_LISTEN` enum value is 1.
    Ok(info.tcpi_state == 1)
}

#[cfg(all(unix, not(target_vendor = "apple")))]
fn inherited_listener_is_listening(fd: RawFd) -> anyhow::Result<bool> {
    let mut listening: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            std::ptr::from_mut(&mut listening).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("read the inherited listener listening state");
    }
    Ok(listening != 0)
}

/// Stage a duplicate of an open descriptor for the handoff: close-on-exec
/// stays SET, so the duplicate can never leak into unrelated children of
/// this daemon (tools, shells, bundles). [`daemon::spawn_handoff`] clears
/// the flag in the successor's `pre_exec`, making the descriptors
/// inheritable only across that one exec.
#[cfg(unix)]
fn stage_for_handoff(source: RawFd) -> anyhow::Result<OwnedFd> {
    let duplicate = unsafe { libc::dup(source) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error()).context("duplicate descriptor");
    }
    let flags = unsafe { libc::fcntl(duplicate, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(duplicate, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(duplicate) };
        return Err(error).context("stage duplicated descriptor");
    }
    // SAFETY: `duplicate` is a fresh, open, owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

/// The staged descriptors of a handoff: listener, database lock, and the
/// optional extra gRPC listener, owned so each closes exactly once. They are
/// created at spawn time — never for the daemon's whole life — and the old
/// generation holds them until it finishes the takeover (health wait, or the
/// park), so the flock and the listening socket survive here even if the
/// successor dies mid-handoff.
#[cfg(unix)]
struct HandoffFds {
    listener: OwnedFd,
    lock: OwnedFd,
    extra: Option<OwnedFd>,
}

#[cfg(unix)]
impl HandoffFds {
    fn stage(listener_fd: RawFd, lock_fd: RawFd, extra_fd: Option<RawFd>) -> anyhow::Result<Self> {
        Ok(Self {
            listener: stage_for_handoff(listener_fd)
                .context("stage the listener for restart handoff")?,
            lock: stage_for_handoff(lock_fd)
                .context("stage the database lock for restart handoff")?,
            extra: match extra_fd {
                Some(fd) => Some(
                    stage_for_handoff(fd)
                        .context("stage the extra gRPC listener for restart handoff")?,
                ),
                None => None,
            },
        })
    }

    fn listener_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    fn lock_fd(&self) -> RawFd {
        self.lock.as_raw_fd()
    }

    fn extra_fd(&self) -> Option<RawFd> {
        self.extra.as_ref().map(AsRawFd::as_raw_fd)
    }
}

/// Validate a supervisor-passed descriptor without owning it: it must be
/// open, an IPv4/IPv6 socket, a stream (`SOCK_STREAM`) socket, and in
/// listening state. A rejected descriptor is left exactly as the supervisor
/// handed it over.
#[cfg(unix)]
fn validate_inherited_listener_fd(fd: RawFd) -> anyhow::Result<()> {
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err(std::io::Error::last_os_error()).context("inherited listener FD is not open");
    }
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    if unsafe { libc::getsockname(fd, std::ptr::from_mut(&mut storage).cast(), &mut length) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("inherited listener FD is not a socket");
    }
    // `sa_family_t` differs per platform (unsigned byte on macOS, unsigned
    // short on Linux); compare through `c_int` like the `AF_*` constants.
    let family = i32::from(storage.ss_family);
    if family != libc::AF_INET && family != libc::AF_INET6 {
        anyhow::bail!("inherited listener FD must be an IPv4 or IPv6 socket");
    }
    let mut kind: libc::c_int = 0;
    let mut kind_length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            std::ptr::from_mut(&mut kind).cast(),
            &mut kind_length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("read the inherited listener socket type");
    }
    if kind != libc::SOCK_STREAM {
        anyhow::bail!(
            "inherited listener FD must be a TCP (stream) socket, got socket type {kind}"
        );
    }
    if !inherited_listener_is_listening(fd)? {
        anyhow::bail!(
            "inherited listener FD must be a listening socket, not a connected or unbound one"
        );
    }
    Ok(())
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
    /// Launch configuration retained for a restart successor.
    handoff_spec: Option<daemon::DaemonSpec>,
    handoff_relay: RelayFlags,
    /// The base agent of this server: the successor resume and the
    /// in-process recovery after a failed successor spawn re-drive
    /// checkpointed turns with the same identity.
    handoff_base: Arc<hya_core::engine::AgentSpec>,
    /// The relay host connector (joined by `--relay` or `hya serve relay
    /// connect`); left after the drain.
    pub(crate) relay: hya_server::RelayHost,
    /// This generation's pinned executable and native libraries: what a
    /// failed restart rolls back to. `None` without a daemon database or
    /// when pinning failed (a failed restart then parks).
    pin: Option<crate::generation_pin::GenerationPin>,
}

/// Serve `prepared` until a termination signal, then tear down — or hand the
/// whole server to a successor (`hya serve restart` with `mode: "handoff"`).
///
/// Plain stop/signal: drain first — every in-flight turn in every session
/// (roots and members) is cancelled with cause `shutdown` and closes its
/// messages within the drain deadline, members go terminal, and new turns
/// are refused — so open turn requests finish and the server can complete
/// its graceful shutdown. The spawn supervisor is shut down afterwards.
///
/// Handoff: acknowledge in the journal (`queued`), close every active turn
/// at a durable handoff boundary, and spawn the successor with staged
/// listener, lock, and extra gRPC descriptors (close-on-exec everywhere
/// except the successor's own exec). Before anything quiesces, the journal's
/// successor executable is resolved and validated; a handoff without one
/// fails without touching the running turns. Live work that cannot reach
/// the boundary rejects the whole restart: the journal records `failed`, the
/// clients are never told anything, and this server keeps serving (the next
/// signal is a plain stop). A failed successor spawn lifts the quiesce and
/// re-drives the already-checkpointed turns in this generation — the server
/// resumes, it does not drain. After a spawned successor the generation
/// shuts its infrastructure down, releases the runtime owner, records
/// `released`, and waits for the successor's health before exiting — if the
/// successor never becomes healthy, this generation parks as the owner of
/// the lock and the listener instead of leaving them headless.
pub(crate) async fn serve_until(
    prepared: PreparedServer,
    signals: TerminationSignals,
) -> anyhow::Result<()> {
    let PreparedServer {
        listener,
        extra,
        server,
        built,
        mut lock,
        streams,
        relay,
        handoff_spec,
        handoff_relay,
        handoff_base,
        pin,
        ..
    } = prepared;
    // The rollback successor rejoins this generation's own relay.
    let rollback_relay = handoff_relay.clone();
    let supervisor = built.resident_supervisor();
    let handoff_engine = built.engine();
    // The engine moves into a cell so the shutdown future (the handoff
    // boundary) and the teardown below can both reach it.
    let handoff_built = Arc::new(tokio::sync::Mutex::new(Some(built)));
    let stop_request = lock.as_ref().map(db_lock::DbLock::stop_request_path);
    // The descriptors a successor inherits, by number: staging happens at
    // spawn time (in the shutdown future), never for the daemon's whole
    // life, so nothing here is inheritable for the daemon's lifetime. The
    // listener, the lock file, and the extra listener stay open at their
    // numbers until the shutdown begins, so the numbers stay valid.
    let handoff_listener_fd = listener.as_raw_fd();
    let handoff_lock_fd = lock.as_ref().map(db_lock::DbLock::lock_raw_fd);
    let handoff_extra_fd = extra.as_ref().map(|extra| extra.as_raw_fd());
    // The signals are shared between the shutdown future and the parked
    // owner path below: both need to be able to wait for the next one.
    let signals = Arc::new(tokio::sync::Mutex::new(signals));
    // Set once the handoff spawn succeeded: the pid, the staged duplicates
    // (kept open through the release and the health wait — the flock and the
    // listening socket survive here even if the successor dies mid-wait),
    // and this handoff's journal token (so a superseded journal is never
    // written to).
    let successor_cell = Arc::new(std::sync::Mutex::new(None::<u32>));
    let successor_fds_cell = Arc::new(std::sync::Mutex::new(None::<HandoffFds>));
    let successor_token_cell = Arc::new(std::sync::Mutex::new(None::<String>));
    let journal_path = handoff_spec
        .as_ref()
        .and_then(|spec| db_lock::paths(&spec.db))
        .map(|paths| paths.handoff);
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
    let parked_signals = Arc::clone(&signals);
    let parked_successor = Arc::clone(&successor_cell);
    let parked_fds = Arc::clone(&successor_fds_cell);
    let parked_token = Arc::clone(&successor_token_cell);
    let closure_built = Arc::clone(&handoff_built);
    // The shutdown future gets its own copies: the originals stay for the
    // teardown below (released stage, successor health, park).
    let closure_handoff_spec = handoff_spec.clone();
    let closure_journal_path = journal_path.clone();
    let closure_base = Arc::clone(&handoff_base);
    let handoff_capable = closure_handoff_spec.is_some() && closure_journal_path.is_some();
    server
        .serve(listener, async move {
            loop {
                let mut signal_guard = signals.lock().await;
                next_signal(&mut signal_guard).await;
                // Why: `hya serve stop|restart` leave a request addressed to
                // this pid before their SIGTERM; anything else is a plain
                // signal.
                let notice = stop_request
                    .as_ref()
                    .and_then(|path| db_lock::take_stop_request(path, std::process::id()))
                    .unwrap_or(db_lock::StopNotice {
                        reason: hya_server::ShutdownReason::Signal,
                        handoff: false,
                    });
                let reason = notice.reason;
                let handoff = notice.handoff
                    && reason == hya_server::ShutdownReason::Restart
                    && handoff_capable;
                if !handoff {
                    stopping.cancel();
                    // End every client's live event stream (SSE and gRPC)
                    // first, with that reason as the last frame: they never
                    // finish on their own, and connected clients (the daemon
                    // outlives them, ADR-0023) must not hold the shutdown
                    // open. Health answers `unavailable` from here on.
                    streams.close(reason);
                    supervisor
                        .drain(hya_proto::FinishCause::Shutdown, hya_core::DRAIN_DEADLINE)
                        .await;
                    return;
                }
                let (Some(spec), Some(journal)) =
                    (closure_handoff_spec.as_ref(), closure_journal_path.as_ref())
                else {
                    continue;
                };

                // A handoff restart: resolve and validate the successor from
                // the journal, stage the descriptors, acknowledge, quiesce,
                // close every active turn at its durable handoff boundary,
                // and only then spawn the successor.
                let reject = |why: String| {
                    let _ = db_lock::write_handoff_stage(
                        journal,
                        db_lock::HandoffStage::Failed,
                        std::process::id(),
                        None,
                        Some(&why),
                    );
                    eprintln!("hya: restart rejected, keeping this server: {why}");
                };
                // The successor executable comes from the journal (the
                // invoking restart CLI's current_exe), never from this
                // generation's own binary — a restart right after an update
                // must bring up the new version. Validated before anything
                // quiesces, so a bad handoff costs the running turns nothing.
                let Some(state) = db_lock::read_handoff(journal) else {
                    reject("the handoff journal disappeared".to_owned());
                    continue;
                };
                let Some(record) = state.spec.clone() else {
                    reject("the handoff journal has no successor spec".to_owned());
                    continue;
                };
                let Some(successor_exe) = record.exe.clone() else {
                    reject("the handoff journal does not name the successor executable".to_owned());
                    continue;
                };
                if let Err(error) = daemon::validate_successor_exe(&successor_exe) {
                    reject(format!("the successor executable is unusable: {error:#}"));
                    continue;
                }
                // The staged descriptors: the single moment duplicates exist
                // in this process. Close-on-exec stays set — only the
                // successor's `pre_exec` clears it, across its one exec.
                let Some(lock_fd) = handoff_lock_fd else {
                    reject("this server holds no database lock to hand off".to_owned());
                    continue;
                };
                let staged = match HandoffFds::stage(handoff_listener_fd, lock_fd, handoff_extra_fd)
                {
                    Ok(staged) => staged,
                    Err(error) => {
                        reject(format!(
                            "could not stage the handoff descriptors: {error:#}"
                        ));
                        continue;
                    }
                };
                let handoff_token = state.token().to_owned();
                if let Err(error) = db_lock::write_handoff_stage(
                    journal,
                    db_lock::HandoffStage::Queued,
                    std::process::id(),
                    None,
                    None,
                ) {
                    reject(format!("could not record the queued stage: {error}"));
                    drop(staged);
                    continue;
                }
                // Pending interaction rows stay durable and are re-exposed by
                // the successor with their stable request IDs. The turn gate
                // still refuses an unsafe handoff while the old oneshot is
                // active; no decision is fabricated here.
                // Quiesce and wait: new turns are refused; each active turn
                // checkpoints itself at its next round boundary. Stragglers
                // reopen the gate and re-drive the already-checkpointed turns
                // in place (inside `handoff_turns`), so a rejection leaves
                // the server serving.
                let report = {
                    let mut guard = closure_built.lock().await;
                    let Some(built) = guard.as_mut() else {
                        reject("the engine is already gone".to_owned());
                        continue;
                    };
                    built.handoff_turns().await
                };
                if !report.stragglers.is_empty() {
                    // Live work did not reach a safe, transferable boundary
                    // within the deadline. Never cancel it and claim success:
                    // reject the restart and keep serving; a later restart
                    // can try again.
                    let pending_asks = handoff_engine.handoff_readiness().pending_asks.len();
                    let sessions: Vec<String> = report
                        .stragglers
                        .iter()
                        .take(5)
                        .map(|id| id.to_string())
                        .collect();
                    reject(format!(
                        "{} active turn(s) did not reach a safe handoff boundary within the \
                         deadline ({}…), {} pending ask(s); the \
                         turns keep running",
                        report.stragglers.len(),
                        sessions.join(", "),
                        pending_asks
                    ));
                    continue;
                }
                // The successor inputs: the journal's overrides (model,
                // allow-hosts, relay, executable — resolved and validated by
                // the restart client) over this generation's own composition.
                let mut successor_spec = spec.clone();
                if record.model.is_some() {
                    successor_spec.model = record.model;
                }
                successor_spec.allow_hosts = record.allow_hosts;
                successor_spec.exe = successor_exe;
                let successor_relay = record
                    .relay
                    .as_ref()
                    .map(daemon::relay_flags_of)
                    .unwrap_or_else(|| handoff_relay.clone());
                let spawn = daemon::SuccessorSpawn {
                    journal: journal.clone(),
                    listener_fd: staged.listener_fd(),
                    lock_fd: staged.lock_fd(),
                    extra_fd: staged.extra_fd(),
                    started_at: db_lock::paths(&spec.db).and_then(|paths| {
                        db_lock::read_discovery(&paths.discovery)
                            .map(|discovery| discovery.started_at)
                    }),
                    first_party_root: None,
                };
                let child = match daemon::spawn_handoff(
                    &successor_spec,
                    &successor_relay,
                    &spawn,
                    &daemon::log_path(&spec.db).unwrap_or_default(),
                ) {
                    Ok(child) => child,
                    Err(error) => {
                        // The turns are durably checkpointed, but the restart
                        // failed: lift the quiesce and re-drive the handed-off
                        // turns in this generation. The server resumes — no
                        // drain, no shutdown; the recorded failure is the
                        // restart client's explicit error.
                        let why = format!("could not spawn the successor: {error:#}");
                        reject(why);
                        drop(staged);
                        let _ = handoff_engine
                            .resume_handed_off_turns(&closure_base, None)
                            .await;
                        eprintln!("hya: restart aborted, this server resumed its handed-off turns");
                        continue;
                    }
                };
                let pid = child.id();
                if let Ok(mut slot) = successor_cell.lock() {
                    *slot = Some(pid);
                }
                if let Ok(mut slot) = successor_fds_cell.lock() {
                    *slot = Some(staged);
                }
                if let Ok(mut slot) = successor_token_cell.lock() {
                    *slot = Some(handoff_token);
                }
                // The handoff is certain: only now do clients learn that
                // this server is going away and that the next one follows.
                stopping.cancel();
                streams.close(hya_server::ShutdownReason::Restart);
                return;
            }
        })
        .await;
    if let Some(extra) = extra_served {
        let _ = extra.await;
    }
    let mut built_guard = handoff_built.lock().await;
    let Some(mut built) = built_guard.take() else {
        return Err(anyhow::anyhow!("handoff teardown engine is missing"));
    };
    drop(built_guard);
    let successor_pid = parked_successor
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    // The staged duplicates live from the spawn until this generation has
    // finished the takeover: through the release, the successor health wait,
    // and any park — the flock and the listening socket survive here even if
    // the successor dies mid-wait. Every exit below drops them.
    let staged_fds = parked_fds.lock().ok().and_then(|mut slot| slot.take());
    let handoff_token = parked_token.lock().ok().and_then(|mut slot| slot.take());
    if let (Some(pid), Some(spec), Some(journal)) =
        (successor_pid, handoff_spec.as_ref(), journal_path.as_ref())
    {
        // A rollback replaces the successor and its journal token.
        let mut pid = pid;
        let mut handoff_token = handoff_token;
        // Is the journal still this handoff's? A newer restart can supersede
        // it at any moment; that journal belongs to the newer attempt and is
        // never written to from here.
        let journal_ours = |journal: &std::path::Path, token: &Option<String>| {
            journal_is_ours(journal, token.as_deref())
        };
        // A spawned successor owns the listener and the lock from here; the
        // staged duplicates stay open in this generation until it is healthy
        // (or until this generation parks), so a failed handoff never leaves
        // them headless.
        relay.shutdown().await;
        // Shut the infrastructure down in place — the spawn supervisor and
        // its residents — before the runtime owner is released. Every turn
        // already closed at its handoff boundary, so this cancels nothing.
        if let Err(error) = built.shutdown().await {
            eprintln!("hya: could not shut the spawn supervisor down for the handoff: {error}");
        }
        // Release the runtime owner — the claim (and its flock) the successor
        // must be able to take — before the successor opens the store: the
        // successor starts opening only after `released` is recorded below.
        if let Err(error) = built
            .engine()
            .store()
            .release_runtime_owner(built.engine().runtime_owner())
        {
            eprintln!("hya: could not release the runtime owner for handoff: {error}");
        }
        drop(built);
        drop(server);
        if journal_ours(journal, &handoff_token) {
            if let Err(error) = db_lock::write_handoff_stage(
                journal,
                db_lock::HandoffStage::Released,
                std::process::id(),
                None,
                None,
            ) {
                eprintln!("hya: could not record the handoff release ({error})");
            }
        } else {
            eprintln!(
                "hya: the handoff journal was superseded by a newer restart; not recording the release"
            );
        }
        let mut outcome = await_successor(journal, handoff_token.as_deref(), pid, &spec.db).await;
        // Rollback: a successor that failed is replaced by this generation's
        // pinned executable over the same inherited listener and lock —
        // clients reconnect to the previous build instead of a parked,
        // silent server. Parking remains the last resort.
        if let Err(failure) = &outcome
            && let (Some(pinned), Some(fds)) = (pin.as_ref(), staged_fds.as_ref())
            && journal_ours(journal, &handoff_token)
        {
            let failure = failure.clone().unwrap_or_else(|| {
                format!(
                    "the successor (pid {pid}) did not become healthy within {} s",
                    SUCCESSOR_READY_WAIT.as_secs()
                )
            });
            stop_failed_successor(pid).await;
            if let Some(lock) = lock.as_mut()
                && let Err(error) = lock.restore_owner_pid()
            {
                eprintln!("hya: could not restore the owner pid before the rollback: {error}");
            }
            match spawn_rollback(
                journal,
                spec,
                &rollback_relay,
                fds,
                pinned.exe(),
                pinned.first_party_root(),
                &failure,
            ) {
                Ok((fallback, token)) => {
                    eprintln!(
                        "hya: restart failed ({failure}); rolling back to {} (pid {fallback})",
                        pinned.exe().display()
                    );
                    pid = fallback;
                    handoff_token = Some(token);
                    outcome =
                        await_successor(journal, handoff_token.as_deref(), pid, &spec.db).await;
                    if outcome.is_ok() {
                        eprintln!(
                            "hya: restart failed ({failure}); rolled back to {}",
                            pinned.exe().display()
                        );
                    }
                }
                Err(error) => eprintln!("hya: could not roll back the failed restart: {error:#}"),
            }
        }
        if outcome.is_ok() {
            // The successor published its own discovery: this generation's
            // drop must leave it alone.
            lock.as_mut()
                .map(db_lock::DbLock::suppress_discovery_removal);
            drop(lock);
            drop(staged_fds);
            return Ok(());
        }
        let failure = outcome.err().flatten();
        // Neither the successor nor the rollback became healthy. This
        // generation keeps the database lock and the listener (parked): a
        // recoverable owner — the staged duplicates hold both. The failure
        // is recorded only while the journal still belongs to this handoff.
        if journal_ours(journal, &handoff_token) && failure.is_none() {
            let _ = db_lock::write_handoff_stage(
                journal,
                db_lock::HandoffStage::Failed,
                std::process::id(),
                None,
                Some(&format!(
                    "the successor (pid {pid}) did not become healthy within {} s; this generation keeps the database lock and the listener",
                    SUCCESSOR_READY_WAIT.as_secs()
                )),
            );
        }
        if let Some(lock) = lock.as_mut()
            && let Err(error) = lock.restore_owner_pid()
        {
            eprintln!("hya: could not restore the parked owner pid: {error}");
        }
        eprintln!(
            "hya: the handoff successor (pid {pid}) did not become healthy{}; this server keeps \
             the database lock and the listener (stopped) — send SIGTERM to stop it, or restart again",
            failure
                .as_deref()
                .map(|error| format!(": {error}"))
                .unwrap_or_default(),
        );
        loop {
            let signal = tokio::select! {
                _ = async {
                    let mut signal_guard = parked_signals.lock().await;
                    next_signal(&mut signal_guard).await;
                } => true,
                _ = tokio::time::sleep(Duration::from_millis(500)) => false,
            };
            if signal {
                // A parked owner stops like any server: lock released (the
                // flock dies with this process). The discovery file is left
                // alone when the successor is serving after all.
                if daemon::running(&spec.db)
                    .await
                    .is_some_and(|found| found.pid == pid)
                {
                    lock.as_mut()
                        .map(db_lock::DbLock::suppress_discovery_removal);
                }
                drop(lock);
                return Ok(());
            }
            // Late recovery: the successor became healthy after all and
            // serves its discovery. (The journal is not consulted here: a
            // superseded journal says nothing about this successor.)
            if daemon::running(&spec.db)
                .await
                .is_some_and(|found| found.pid == pid)
            {
                lock.as_mut()
                    .map(db_lock::DbLock::suppress_discovery_removal);
                drop(lock);
                return Ok(());
            }
        }
    }
    relay.shutdown().await;
    let shutdown_result = built.shutdown().await.context("shutdown spawn supervisor");
    // Release the runtime owner (its flock) before the database lock: a
    // starter spawns the next daemon as soon as the database lock is free,
    // and that daemon claims the runtime owner first thing. Engine clones in
    // tasks still winding down would otherwise keep the flock past this point.
    if let Err(error) = built
        .engine()
        .store()
        .release_runtime_owner(built.engine().runtime_owner())
    {
        eprintln!("hya: could not release the runtime owner at shutdown: {error}");
    }
    drop(built);
    // Last: remove the discovery file and release the lock.
    drop(lock);
    shutdown_result
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

#[cfg(test)]
mod runtime_owner_busy_tests {
    use super::runtime_owner_busy;

    #[test]
    fn a_held_runtime_owner_anywhere_in_the_chain_is_busy() {
        let busy = anyhow::Error::new(hya_store::StoreError::RuntimeOwnerBusy)
            .context("claim runtime owner before startup recovery");
        assert!(runtime_owner_busy(&busy));
        let other = anyhow::anyhow!("bind 127.0.0.1:0").context("listen");
        assert!(!runtime_owner_busy(&other));
    }
}

/// Whether the handoff journal still belongs to the handoff with `token`
/// (a newer restart supersedes it and is never written to from here).
fn journal_is_ours(journal: &std::path::Path, token: Option<&str>) -> bool {
    match (token, db_lock::read_handoff(journal)) {
        (Some(token), Some(state)) => state.token() == token,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// Wait until the successor `pid` is ready: `Ok` once the journal records its
/// `ready` (or it serves the discovery of `db`), `Err(reason)` when it records
/// `failed`, exits, or the ready wait runs out (`Err(None)`).
///
/// The successor records `ready` after publishing discovery and completing
/// bootstrap. A quick `stop` may remove its discovery before the next health
/// probe; that is a completed handoff, not a failed bootstrap — the journal
/// is the durable readiness evidence in that race.
async fn await_successor(
    journal: &std::path::Path,
    token: Option<&str>,
    pid: u32,
    db: &str,
) -> Result<(), Option<String>> {
    let deadline = tokio::time::Instant::now() + SUCCESSOR_READY_WAIT;
    loop {
        if let Some(state) = db_lock::read_handoff(journal)
            && journal_is_ours(journal, token)
        {
            if matches!(
                state.stage(),
                db_lock::HandoffStage::Ready | db_lock::HandoffStage::Transferred
            ) && state.stage_pid(db_lock::HandoffStage::Ready) == Some(pid)
            {
                return Ok(());
            }
            if state.stage() == db_lock::HandoffStage::Failed {
                return Err(state.error.clone());
            }
        }
        if daemon::running(db)
            .await
            .is_some_and(|found| found.pid == pid)
        {
            return Ok(());
        }
        if let Some(status) = reap(pid) {
            // It may have recorded its failure just before exiting.
            if let Some(state) = db_lock::read_handoff(journal)
                && journal_is_ours(journal, token)
                && state.stage() == db_lock::HandoffStage::Failed
            {
                return Err(state.error.clone());
            }
            return Err(Some(format!(
                "the successor (pid {pid}) exited ({status}) before it became ready"
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(None);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Reap the child `pid` if it has exited: its exit status in words.
fn reap(pid: u32) -> Option<String> {
    let pid = i32::try_from(pid).ok()?;
    let mut status = 0;
    // SAFETY: `waitpid` with WNOHANG only reads the child's status.
    let reaped = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
    if reaped != pid {
        return None;
    }
    Some(if libc::WIFEXITED(status) {
        format!("exit {}", libc::WEXITSTATUS(status))
    } else {
        format!("signal {}", libc::WTERMSIG(status))
    })
}

/// Kill a failed successor and wait until it is gone, so its runtime-owner
/// claim and its copies of the inherited descriptors are released before
/// the rollback successor starts.
async fn stop_failed_successor(pid: u32) {
    if reap(pid).is_some() || !daemon::process_alive(pid) {
        return;
    }
    if let Ok(raw) = i32::try_from(pid) {
        // SAFETY: `kill` has no memory-safety preconditions.
        unsafe {
            libc::kill(raw, libc::SIGKILL);
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if reap(pid).is_some() || !daemon::process_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eprintln!("hya: the failed successor (pid {pid}) did not exit after SIGKILL");
}

/// Start the rollback successor: this generation's pinned executable with
/// its own composition (`spec`, `relay`) over the staged descriptors, through
/// a fresh handoff journal (`requested` naming the pinned executable and the
/// failed attempt, then `queued` and `released` by this generation, whose
/// runtime is already released). Returns the rollback pid and journal token.
fn spawn_rollback(
    journal: &std::path::Path,
    spec: &daemon::DaemonSpec,
    relay: &RelayFlags,
    fds: &HandoffFds,
    pinned: &std::path::Path,
    pinned_first_party: Option<&std::path::Path>,
    failure: &str,
) -> anyhow::Result<(u32, String)> {
    let me = std::process::id();
    let record = db_lock::HandoffSpec {
        model: spec.model.clone(),
        allow_hosts: spec.allow_hosts.clone(),
        relay: daemon::relay_spec(relay),
        exe: Some(pinned.to_path_buf()),
        rolled_back_from: Some(failure.to_owned()),
    };
    db_lock::write_handoff_stage(
        journal,
        db_lock::HandoffStage::Requested,
        me,
        Some(&record),
        None,
    )
    .context("record the rollback request")?;
    for stage in [
        db_lock::HandoffStage::Queued,
        db_lock::HandoffStage::Released,
    ] {
        db_lock::write_handoff_stage(journal, stage, me, None, None)
            .context("record the rollback release")?;
    }
    let token = db_lock::read_handoff(journal)
        .context("read the rollback journal")?
        .token()
        .to_owned();
    let mut fallback = spec.clone();
    fallback.exe = pinned.to_path_buf();
    let spawn = daemon::SuccessorSpawn {
        journal: journal.to_path_buf(),
        listener_fd: fds.listener_fd(),
        lock_fd: fds.lock_fd(),
        extra_fd: fds.extra_fd(),
        started_at: db_lock::paths(&spec.db).and_then(|paths| {
            db_lock::read_discovery(&paths.discovery).map(|discovery| discovery.started_at)
        }),
        first_party_root: pinned_first_party.map(std::path::Path::to_path_buf),
    };
    let child = daemon::spawn_handoff(
        &fallback,
        relay,
        &spawn,
        &daemon::log_path(&spec.db).unwrap_or_default(),
    )
    .context("spawn the rollback successor")?;
    Ok((child.id(), token))
}

/// Compose the runtime and use either `bind` or a supervisor-owned listener.
///
/// An inherited listener is consumed exactly once. It is only intended for a
/// foreground server that is being launched by a supervisor; daemon control
/// actions continue to own their existing bind/restart lifecycle.
///
/// `lock` is the caller's claim on `db` ([`db_lock::try_claim`]); once the
/// listener is ready its discovery file is published.
// The composition inputs of one server; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_server(
    bind: &str,
    inherited: Option<std::net::TcpListener>,
    db: String,
    mut lock: Option<db_lock::DbLock>,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    relay_flags: &RelayFlags,
    hosts: &hya_server::HostPolicy,
    handoff_spec: Option<daemon::DaemonSpec>,
    handoff_relay: RelayFlags,
    inherited_extra: Option<std::net::TcpListener>,
    resume_fatal: bool,
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
    let agent = Arc::new(agent_base_with_model(&runtime.model, None));
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
    let mut state = AppState::new(Arc::clone(&engine), Arc::clone(&agent))
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
    let listener = listener_from_inherited_or_bind(inherited, bind).await?;
    let addr = listener.local_addr().context("read local addr")?;
    // The extra gRPC listener: transferred from the old generation in a
    // handoff, else bound from the legacy `HYA_GRPC_BIND` environment.
    let extra = match inherited_extra {
        Some(extra) => match tokio::net::TcpListener::from_std(extra) {
            Ok(extra) => Some(extra),
            Err(error) => {
                eprintln!("hya: could not adopt the inherited gRPC listener ({error})");
                None
            }
        },
        None => match std::env::var("HYA_GRPC_BIND")
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
        },
    };
    let extra_url = extra
        .as_ref()
        .and_then(|extra| extra.local_addr().ok())
        .map(|addr| format!("http://{addr}"));
    // Build the complete shared server before advertising readiness.
    let server = hya_server::build(state.clone());
    relay.set_service(server.clone());
    // Successor side of a restart handoff: resume what the old generation
    // closed at its handoff boundary, before this generation publishes its
    // discovery (`ready` means fully resumed, not half-booted).
    let resumed = run_handoff_resume(&built, &agent, resume_fatal).await?;
    if !resumed.is_empty() {
        eprintln!("hya: resumed {} handed-off session turn(s)", resumed.len());
    }
    resume_handed_off_interactions(&built, &agent, resume_fatal).await?;
    if let Some(lock) = lock.as_mut() {
        lock.publish_with(&db_lock::connect_url(addr), &hosts.extra_hosts())
            .context("publish the server discovery file")?;
    }
    // Pin the running generation (executable + the native libraries it
    // loaded while composing) so a failed restart can roll back to it.
    let pin = handoff_spec
        .as_ref()
        .and_then(|spec| match crate::generation_pin::pin(&spec.db) {
            Ok(pin) => Some(pin),
            Err(error) => {
                eprintln!("hya: could not pin this generation; a failed restart will park instead of rolling back ({error:#})");
                None
            }
        });
    Ok(PreparedServer {
        pin,
        url: format!("http://{addr}"),
        listener,
        extra,
        extra_url,
        streams: state.streams(),
        server,
        built,
        lock,
        handoff_spec,
        handoff_relay,
        handoff_base: Arc::clone(&agent),
        relay,
    })
}

/// Deliver answers submitted while the previous generation was transferring.
///
/// Replies are claimed in the store before continuation. This explicit
/// at-most-once fence prevents a successor crash from replaying side effects;
/// continuation only appends a terminal tool event and never invokes the old
/// interrupted tool again.
async fn resume_handed_off_interactions(
    built: &hya_app::BuiltSessionEngine,
    base: &hya_core::engine::AgentSpec,
    resume_fatal: bool,
) -> anyhow::Result<()> {
    let engine = built.engine();
    let store = engine.store();
    let replies = store
        .list_pending_interaction_replies()
        .await
        .context("list handed-off interaction replies")?;
    if replies.is_empty() {
        return Ok(());
    }
    let pending = store
        .list_pending_interactions()
        .await
        .context("list handed-off interactions")?;
    for reply in replies {
        let Some(interaction) = pending.iter().find(|row| row.id == reply.id) else {
            // A concurrent in-process owner already resolved this request.
            let _ = store.claim_pending_interaction_reply(&reply.id).await?;
            continue;
        };
        let Some(session) = interaction.session else {
            continue_interaction_failure(
                resume_fatal,
                format!("handed-off interaction {} has no session", reply.id),
            )?;
            continue;
        };
        let events = store
            .replay(session)
            .await
            .with_context(|| format!("replay session {session} for handed-off interaction"))?;
        let (message, part, call) = match interaction_tool_target(&events, &interaction.payload) {
            Some(target) => target,
            None => {
                continue_interaction_failure(
                    resume_fatal,
                    format!(
                        "could not unambiguously correlate handed-off interaction {}",
                        reply.id
                    ),
                )?;
                continue;
            }
        };
        let value: serde_json::Value = match serde_json::from_str(&reply.payload) {
            Ok(value) => value,
            Err(error) => {
                continue_interaction_failure(
                    resume_fatal,
                    format!("invalid reply {}: {error}", reply.id),
                )?;
                continue;
            }
        };
        // Claim before driving: a crash after this fence cannot replay a tool
        // side effect. The continuation itself only appends a terminal event.
        if !store
            .claim_pending_interaction_reply(&reply.id)
            .await
            .with_context(|| format!("claim handed-off interaction reply {}", reply.id))?
        {
            continue;
        }
        // A permission allow is represented as a successful synthetic result;
        // deny remains a tool error. Questions preserve the submitted answer
        // batch as the tool result. Neither path re-runs the old tool call.
        let continuation = if interaction.kind == "permission" {
            match value.get("reply").and_then(serde_json::Value::as_str) {
                Some("once") | Some("always") => engine.continue_after_handoff_tool_result(
                    session, message, part, call,
                    serde_json::json!({"handoff": "permission_allowed", "reply": value.get("reply")}),
                    0, base,
                ).await,
                Some("reject") => engine.continue_after_handoff_tool_error(
                    session, message, part, call,
                    value.get("message").and_then(serde_json::Value::as_str)
                        .unwrap_or("permission denied"), base,
                ).await,
                _ => engine.continue_after_handoff_tool_error(
                    session, message, part, call, "invalid permission reply", base,
                ).await,
            }
        } else if value
            .get("reject")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            engine
                .continue_after_handoff_tool_error(
                    session,
                    message,
                    part,
                    call,
                    "question cancelled",
                    base,
                )
                .await
        } else {
            engine
                .continue_after_handoff_tool_result(
                    session,
                    message,
                    part,
                    call,
                    serde_json::json!({
                        "title": "Asked questions",
                        "output": "Questions answered during daemon handoff",
                        "metadata": { "answers": value.get("answers").cloned().unwrap_or(serde_json::Value::Null) },
                    }),
                    0,
                    base,
                )
                .await
        };
        match continuation {
            Ok(_) => {}
            Err(error) => continue_interaction_failure(
                resume_fatal,
                format!("resume handed-off interaction {}: {error:#}", reply.id),
            )?,
        }
    }
    Ok(())
}

fn continue_interaction_failure(fatal: bool, message: String) -> anyhow::Result<()> {
    if fatal {
        Err(anyhow::anyhow!(message))
    } else {
        eprintln!("hya: {message}; the reply was already fenced and will not be replayed");
        Ok(())
    }
}

fn interaction_tool_target(
    events: &[hya_proto::Envelope],
    payload: &str,
) -> Option<(MessageId, PartId, ToolCallId)> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let tool = value.pointer("/properties/tool");
    let message = tool
        .and_then(|v| v.get("messageID"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_id::<MessageId>);
    let call = tool
        .and_then(|v| v.get("callID"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_id::<ToolCallId>);
    let mut open = std::collections::BTreeMap::<ToolCallId, (MessageId, PartId)>::new();
    for envelope in events {
        match envelope.event {
            Event::ToolCallRequested {
                message: requested_message,
                part,
                call: requested_call,
                ..
            } => {
                if message.is_none_or(|id| id == requested_message) {
                    open.insert(requested_call, (requested_message, part));
                }
            }
            Event::ToolResult { call, .. } | Event::ToolError { call, .. } => {
                open.remove(&call);
            }
            _ => {}
        }
    }
    if let Some(call) = call {
        return open
            .get(&call)
            .copied()
            .map(|(message, part)| (message, part, call));
    }
    // Question payloads omit tool IDs. Infer only for one unambiguous call.
    if open.len() != 1 {
        return None;
    }
    open.into_iter()
        .next()
        .map(|(call, (message, part))| (message, part, call))
}

fn parse_id<T>(value: &str) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
}

/// Successor-side handoff hook: resume the sessions the old generation
/// closed with `cause: handoff`
/// ([`SessionEngine::resume_handed_off_turns`]). This is the single
/// integration point for the resume driver — different base-agent identity,
/// request-scoped guidance, or suspend/abort wiring change this function
/// only. Bounded by [`HANDOFF_RESUME_BOUND`]: a handoff successor refuses to
/// serve half-resumed (`resume_fatal`), a plain start treats the resume as
/// crash recovery and only warns.
async fn run_handoff_resume(
    built: &hya_app::BuiltSessionEngine,
    base: &hya_core::engine::AgentSpec,
    resume_fatal: bool,
) -> anyhow::Result<Vec<hya_proto::SessionId>> {
    let engine = built.engine();
    let resumed = tokio::time::timeout(
        HANDOFF_RESUME_BOUND,
        engine.resume_handed_off_turns(base, None),
    )
    .await;
    match resumed {
        Ok(resumed) => Ok(resumed),
        Err(_) if resume_fatal => Err(anyhow::anyhow!(
            "the handoff resume did not finish within {} s",
            HANDOFF_RESUME_BOUND.as_secs()
        )),
        Err(_) => {
            eprintln!(
                "hya: the handoff resume did not finish within {} s; continuing with crash recovery",
                HANDOFF_RESUME_BOUND.as_secs()
            );
            Ok(Vec::new())
        }
    }
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

/// Resolve once any of the registered stop signals fires. Unlike a one-shot
/// future this can be awaited again: a rejected handoff keeps serving and
/// still stops at the next signal.
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

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod listener_tests {
    use super::inherited_std_listener;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

    #[test]
    fn rejects_standard_stream_fds() {
        let error = inherited_std_listener(2).expect_err("stderr is not a listener");
        assert!(error.to_string().contains("must be >= 3"), "{error:#}");
    }

    /// Regression: a closed FD must be rejected with a normal error before
    /// ownership is taken. The old path adopted an invalid descriptor with
    /// `from_raw_fd` and aborted the whole process (`IO Safety violation`)
    /// when dropping it closed the bad FD.
    #[test]
    fn rejects_closed_fd_with_normal_error() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind scratch socket");
        let fd = socket.into_raw_fd();
        drop(unsafe { OwnedFd::from_raw_fd(fd) }); // close it: the number names nothing now
        let error = inherited_std_listener(u32::try_from(fd).expect("test fd fits u32"))
            .expect_err("a closed FD must be rejected");
        assert!(error.to_string().contains("not open"), "{error:#}");
    }

    #[test]
    fn rejects_udp_socket() {
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind test socket");
        let fd = udp.into_raw_fd();
        let error = inherited_std_listener(u32::try_from(fd).expect("test fd fits u32"))
            .expect_err("a UDP socket is not a TCP listener");
        assert!(error.to_string().contains("stream"), "{error:#}");
        // Fail closed: a rejected FD stays open for its owner; the test closes it.
        assert!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "rejected FD must be left open for its owner"
        );
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }

    #[test]
    fn rejects_connected_tcp_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let stream =
            std::net::TcpStream::connect(listener.local_addr().expect("read test address"))
                .expect("connect test stream");
        let fd = stream.into_raw_fd();
        let error = inherited_std_listener(u32::try_from(fd).expect("test fd fits u32"))
            .expect_err("a connected socket is not listening");
        assert!(error.to_string().contains("listening"), "{error:#}");
        assert!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "rejected FD must be left open for its owner"
        );
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }

    #[test]
    fn rejects_non_inet_listener() {
        let path =
            std::env::temp_dir().join(format!("hya-listener-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let unix = std::os::unix::net::UnixListener::bind(&path).expect("bind test unix listener");
        let fd = unix.into_raw_fd();
        let error = inherited_std_listener(u32::try_from(fd).expect("test fd fits u32"))
            .expect_err("a Unix-domain listener is not TCP");
        assert!(error.to_string().contains("IPv4 or IPv6"), "{error:#}");
        assert!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "rejected FD must be left open for its owner"
        );
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn adopts_listener_and_marks_it_close_on_exec() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let expected = listener.local_addr().expect("read test listener address");
        listener
            .set_nonblocking(true)
            .expect("set test listener nonblocking");
        let fd = listener.into_raw_fd();
        let adopted = inherited_std_listener(u32::try_from(fd).expect("test fd fits u32"))
            .expect("adopt inherited listener");
        assert_eq!(
            adopted.local_addr().expect("read adopted address"),
            expected
        );
        let flags = unsafe { libc::fcntl(adopted.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "read close-on-exec flags");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "listener must not leak to children"
        );
    }

    /// The captured std listener converts to Tokio exactly once and still
    /// accepts real connections.
    #[tokio::test]
    async fn adopts_listener_and_accepts_a_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let expected = listener.local_addr().expect("read test listener address");
        listener
            .set_nonblocking(true)
            .expect("set test listener nonblocking");
        let fd = listener.into_raw_fd();
        let adopted = inherited_std_listener(u32::try_from(fd).expect("test fd fits u32"))
            .expect("adopt inherited listener");
        let listener = tokio::net::TcpListener::from_std(adopted).expect("convert to Tokio");
        assert_eq!(
            listener.local_addr().expect("read converted address"),
            expected
        );
        let client = tokio::net::TcpStream::connect(expected)
            .await
            .expect("connect");
        let (accepted, _) = listener.accept().await.expect("accept a connection");
        assert_eq!(
            accepted.peer_addr().expect("read accepted peer"),
            client.local_addr().expect("read client address")
        );
    }
}
