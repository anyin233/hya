use std::sync::Arc;

use anyhow::Context as _;
use hya_server::{AppState, router as server_router};

use super::{agent_base_with_model, build_session_engine, open_store, resolve_runtime};

pub(crate) async fn cmd_serve(
    bind: String,
    db: String,
    model_override: Option<String>,
    yolo: bool,
) -> anyhow::Result<()> {
    super::first_run_config_bootstrap(false)?;
    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override).await.with_yolo(yolo);
    // Server AppState: base-only agent slot. Environment + AGENTS + references
    // are discovered per turn so Bundle Some does not drop project AGENTS and
    // Bundle None does not duplicate startup-baked AGENTS.
    let agent = Arc::new(agent_base_with_model(&runtime.model, runtime.reasoning));
    let mut built = build_session_engine(
        store,
        runtime.router,
        agent.as_ref(),
        runtime.mcp,
        runtime.plugins,
        (runtime.websearch, runtime.permission),
    )
    .await?;
    let engine = built.engine();
    let asks = built
        .take_asks()
        .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
    let questions = built
        .take_questions()
        .ok_or_else(|| anyhow::anyhow!("questions receiver missing"))?;
    let mcp_control = built.mcp_control();
    let workflow_control = Arc::new(built.workflow_control());
    let plugin_host = built.plugin_host();
    let mut state = AppState::new(engine, agent)
        .with_question_requests(questions)
        .with_mcp_control(mcp_control)
        .with_workflow_control(workflow_control)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone());
    if yolo {
        eprintln!("hya: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
    }
    state = state.with_permission_requests(asks);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr().context("read local addr")?;
    let url = format!("http://{addr}");
    // Install the termination handlers BEFORE announcing readiness. Callers that parse the
    // listen line and then signal us (the e2e harness) would otherwise race handler setup,
    // and losing that race means the default disposition kills the process outright —
    // measured: SIGTERM 7ms after the listen line died by signal, 500ms after it exited 0.
    let terminate = install_termination_signals().context("install termination handlers")?;
    println!("hya server listening on {url}");
    emit_startup_mark("backend_listen", Some(&url));
    // Without a shutdown future `axum::serve` never returns, so `built.shutdown()` below
    // would be unreachable and the process could only ever die by signal — skipping atexit
    // handlers (and therefore any coverage/profile flush). Handing it SIGTERM/Ctrl-C makes
    // the already-written teardown path run and lets `main` return normally.
    let serve_result = axum::serve(listener, server_router(state))
        .with_graceful_shutdown(wait_for_termination(terminate))
        .await
        .context("serve http");
    let shutdown_result = built.shutdown().await.context("shutdown spawn supervisor");
    serve_result.and(shutdown_result)
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
/// must keep them until shutdown. Mirrors the idiom in `crates/hya-ts/src/main.rs`, except
/// registration is eager rather than on first poll (see the race note in `cmd_serve`).
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
    let enabled = std::env::var_os("HYA_STARTUP_TRACE")
        .map(|value| {
            let text = value.to_string_lossy();
            text.eq_ignore_ascii_case("1") || text.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let wall_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    match detail {
        Some(detail) => {
            let escaped = detail.replace('\\', "\\\\").replace('"', "\\\"");
            eprintln!(
                r#"{{"hya_startup":true,"mark":"{mark}","wall_ms":{wall_ms},"detail":"{escaped}"}}"#
            );
        }
        None => eprintln!(r#"{{"hya_startup":true,"mark":"{mark}","wall_ms":{wall_ms}}}"#),
    }
}

/// Default `hya`: run the HTTP/SSE backend in-process on an ephemeral loopback port and hand the
/// terminal to the current `hya` frontend.
pub(crate) async fn cmd_tui_hya(
    model_override: Option<String>,
    db: String,
    yolo: bool,
    resume: Option<String>,
) -> anyhow::Result<()> {
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() {
        println!(
            "hya {} — a multi-agent coding agent",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "The hya frontend needs a terminal. Try `hya-backend exec \"<prompt>\"`, \
             `hya-backend -p \"<goal>\"`, or `hya-backend --help`."
        );
        return Ok(());
    }

    super::first_run_config_bootstrap(true)?;
    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override).await.with_yolo(yolo);
    // Interactive startup (stdout is a terminal, checked above): explain the
    // missing config and the offline fallback. Goes to stderr only.
    if let Some(notice) = &runtime.offline_notice {
        notice.emit();
    }
    // Interactive TUI backend uses the same base-only AppState seam as serve.
    let agent = Arc::new(agent_base_with_model(&runtime.model, runtime.reasoning));
    let mut built = build_session_engine(
        store,
        runtime.router,
        agent.as_ref(),
        runtime.mcp,
        runtime.plugins,
        (runtime.websearch, runtime.permission),
    )
    .await?;
    let engine = built.engine();
    let asks = built
        .take_asks()
        .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
    let questions = built
        .take_questions()
        .ok_or_else(|| anyhow::anyhow!("questions receiver missing"))?;
    let mcp_control = built.mcp_control();
    let workflow_control = Arc::new(built.workflow_control());
    let plugin_host = built.plugin_host();
    let mut state = AppState::new(engine, agent)
        .with_question_requests(questions)
        .with_mcp_control(mcp_control)
        .with_workflow_control(workflow_control)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone());
    if yolo {
        eprintln!("hya: --yolo auto-approves ALL tool actions for the hya frontend (RCE risk)");
    }
    state = state.with_permission_requests(asks);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind ephemeral backend port")?;
    let base_url = format!(
        "http://{}",
        listener.local_addr().context("read local addr")?
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, server_router(state)).await;
    });

    let result = launch_hya(&base_url, resume.as_deref()).await;
    server.abort();
    let _ = server.await;
    drop(plugin_host);
    let shutdown_result = built.shutdown().await.context("shutdown spawn supervisor");
    match (result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
    }
}

async fn launch_hya(base_url: &str, resume: Option<&str>) -> anyhow::Result<()> {
    let bin = resolve_hya_bin();
    let status = tokio::process::Command::new(&bin)
        .args(hya_launch_args(base_url, resume))
        .status()
        .await
        .with_context(|| {
            format!("launch hya frontend `{bin}` (set HYA_FRONTEND_BIN to override)")
        })?;
    if !status.success() {
        anyhow::bail!("hya frontend exited with {status}");
    }
    Ok(())
}

fn hya_launch_args(base_url: &str, resume: Option<&str>) -> Vec<String> {
    let mut args = vec!["--server".to_string(), base_url.to_string()];
    if let Some(session) = resume {
        args.push("--session".to_string());
        args.push(session.to_string());
    }
    args
}

/// Resolve the `hya` binary: `HYA_FRONTEND_BIN`, then the most recently built workspace
/// `target/{release,debug}/hya` (newest wins so a stale build never shadows a fresh one),
/// then `hya` on `PATH`.
fn resolve_hya_bin() -> String {
    if let Ok(bin) = std::env::var("HYA_FRONTEND_BIN") {
        return bin;
    }
    let newest = ["release", "debug"]
        .iter()
        .filter_map(|profile| {
            let path = workspace_target_bin(profile, "hya").canonicalize().ok()?;
            let mtime = path.metadata().ok()?.modified().ok()?;
            Some((path, mtime))
        })
        .max_by_key(|(_, mtime)| *mtime);
    if let Some((path, _)) = newest {
        return path.display().to_string();
    }
    "hya".to_string()
}

fn workspace_target_bin(profile: &str, bin: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(profile)
        .join(bin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn workspace_target_bin_points_at_current_workspace_target() {
        let path = workspace_target_bin("debug", "hya");

        assert_eq!(
            path,
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target")
                .join("debug")
                .join("hya")
        );
    }

    #[test]
    fn hya_launch_args_translates_resume_to_frontend_session() {
        let args = hya_launch_args("http://127.0.0.1:1234", Some("hysec_abcdefghijklmnopqrst"));

        assert_eq!(
            args,
            [
                "--server",
                "http://127.0.0.1:1234",
                "--session",
                "hysec_abcdefghijklmnopqrst",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }
}
