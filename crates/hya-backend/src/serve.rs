use std::sync::Arc;

use anyhow::Context as _;
use hya_server::{AppState, router as server_router};

use super::{agent_with_model, build_session_engine, open_store, resolve_runtime};

pub(crate) async fn cmd_serve(
    bind: String,
    db: String,
    model_override: Option<String>,
    yolo: bool,
) -> anyhow::Result<()> {
    super::first_run_config_bootstrap(false)?;
    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override).with_yolo(yolo);
    let agent = Arc::new(agent_with_model(&runtime.model, runtime.reasoning));
    let (engine, asks, questions, mcp_manager, plugin_host) = build_session_engine(
        store,
        runtime.router,
        agent.as_ref(),
        runtime.mcp,
        runtime.plugins,
        runtime.permission,
        true,
    )
    .await;
    let mut state = AppState::new(engine, agent)
        .with_question_requests(questions)
        .with_mcp_manager(mcp_manager)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone())
        .with_global_agents(true);
    if yolo {
        eprintln!("hya: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
    }
    state = state.with_permission_requests(asks);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr().context("read local addr")?;
    println!("hya server listening on http://{addr}");
    axum::serve(listener, server_router(state))
        .await
        .context("serve http")?;
    Ok(())
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
    let runtime = resolve_runtime(model_override).with_yolo(yolo);
    // Interactive startup (stdout is a terminal, checked above): explain the
    // missing config and the offline fallback. Goes to stderr only.
    if let Some(notice) = &runtime.offline_notice {
        notice.emit();
    }
    let agent = Arc::new(agent_with_model(&runtime.model, runtime.reasoning));
    let (engine, asks, questions, mcp_manager, plugin_host) = build_session_engine(
        store,
        runtime.router,
        agent.as_ref(),
        runtime.mcp,
        runtime.plugins,
        runtime.permission,
        true,
    )
    .await;
    let mut state = AppState::new(engine, agent)
        .with_question_requests(questions)
        .with_mcp_manager(mcp_manager)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone())
        .with_global_agents(true);
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
    drop(plugin_host);
    result
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
        args.push("--resume".to_string());
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
    fn hya_launch_args_forwards_resume_session() {
        let args = hya_launch_args("http://127.0.0.1:1234", Some("hysec_abcdefghijklmnopqrst"));

        assert_eq!(
            args,
            [
                "--server",
                "http://127.0.0.1:1234",
                "--resume",
                "hysec_abcdefghijklmnopqrst",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }
}
