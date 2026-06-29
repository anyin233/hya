use std::sync::Arc;

use anyhow::Context as _;
use hya_server::{AppState, router as server_router};

use crate::permission::{PermissionPolicy, spawn_auto_responder};

use super::{agent_with_model, build_session_engine, open_store, resolve_runtime};

pub(crate) async fn cmd_serve(
    bind: String,
    db: String,
    model_override: Option<String>,
    yolo: bool,
) -> anyhow::Result<()> {
    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override);
    let (engine, asks, questions, mcp_manager, plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    )
    .await;
    let mut state = AppState::new(engine, Arc::new(agent_with_model(&runtime.model)))
        .with_question_requests(questions)
        .with_mcp_manager(mcp_manager)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone())
        .with_global_agents(true);
    let _responder = if yolo {
        eprintln!("hya: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
        Some(spawn_auto_responder(asks, PermissionPolicy::Yolo))
    } else {
        state = state.with_permission_requests(asks);
        None
    };
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
/// terminal to the `hya` frontend (the replacement for `hya-render-tui`). The legacy TUI stays reachable
/// via `hya --mini`.
pub(crate) async fn cmd_tui_hya(
    model_override: Option<String>,
    db: String,
    yolo: bool,
) -> anyhow::Result<()> {
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() {
        println!(
            "hya {} — a multi-agent coding agent",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "The hya frontend needs a terminal. Try `hya exec \"<prompt>\"`, \
             `hya -p \"<goal>\"`, or `hya --help`."
        );
        return Ok(());
    }

    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override);
    // Interactive startup (stdout is a terminal, checked above): explain the
    // missing config and the offline fallback. Goes to stderr only.
    if let Some(notice) = &runtime.offline_notice {
        notice.emit();
    }
    let (engine, asks, questions, mcp_manager, plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    )
    .await;
    let mut state = AppState::new(engine, Arc::new(agent_with_model(&runtime.model)))
        .with_question_requests(questions)
        .with_mcp_manager(mcp_manager)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone())
        .with_global_agents(true);
    let _responder = if yolo {
        eprintln!("hya: --yolo auto-approves ALL tool actions for the hya frontend (RCE risk)");
        Some(spawn_auto_responder(asks, PermissionPolicy::Yolo))
    } else {
        state = state.with_permission_requests(asks);
        None
    };

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

    let result = launch_hya(&base_url).await;
    server.abort();
    drop(plugin_host);
    result
}

async fn launch_hya(base_url: &str) -> anyhow::Result<()> {
    let bin = resolve_hya_bin();
    let status = tokio::process::Command::new(&bin)
        .arg("--server")
        .arg(base_url)
        .status()
        .await
        .with_context(|| format!("launch hya frontend `{bin}` (set HYA_HYA_BIN to override)"))?;
    if !status.success() {
        anyhow::bail!("hya frontend exited with {status}");
    }
    Ok(())
}

/// Resolve the `hya` binary: `HYA_HYA_BIN`, then the most recently built sibling
/// `opencode-frontent-rs/target/{release,debug}/hya` (newest wins so a stale build never shadows a
/// fresh one), then `hya` on `PATH`.
fn resolve_hya_bin() -> String {
    if let Ok(bin) = std::env::var("HYA_HYA_BIN") {
        return bin;
    }
    let newest = ["release", "debug"]
        .iter()
        .filter_map(|profile| {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!(
                    "../../../opencode-frontent-rs/target/{profile}/hya"
                ))
                .canonicalize()
                .ok()?;
            let mtime = path.metadata().ok()?.modified().ok()?;
            Some((path, mtime))
        })
        .max_by_key(|(_, mtime)| *mtime);
    if let Some((path, _)) = newest {
        return path.display().to_string();
    }
    "hya".to_string()
}
