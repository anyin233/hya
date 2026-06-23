use std::sync::Arc;

use anyhow::Context as _;
use yaca_server::{AppState, router as server_router};

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
    );
    let mut state = AppState::new(engine, Arc::new(agent_with_model(&runtime.model)))
        .with_question_requests(questions)
        .with_mcp_manager(mcp_manager)
        .with_workspace_adapters(plugin_host.workspace_adapters());
    let _responder = if yolo {
        eprintln!("yaca: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
        Some(spawn_auto_responder(asks, PermissionPolicy::Yolo))
    } else {
        state = state.with_permission_requests(asks);
        None
    };
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr().context("read local addr")?;
    println!("yaca server listening on http://{addr}");
    axum::serve(listener, server_router(state))
        .await
        .context("serve http")?;
    Ok(())
}
