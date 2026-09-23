use std::sync::Arc;

use anyhow::Context as _;
use hya_server::{AppState, router as server_router};

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
) -> anyhow::Result<()> {
    super::first_run_config_bootstrap(false)?;
    let store = open_store(&db).await?;
    let mut runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
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
    let mut state = AppState::new(Arc::clone(&engine), agent)
        .with_question_requests(questions)
        .with_mcp_control(mcp_control)
        .with_workflow_control(workflow_control)
        .with_agent_model_control(agent_model_control)
        .with_workspace_adapters(plugin_host.workspace_adapters())
        .with_default_agent(runtime.default_agent.clone())
        .with_pure_guidance(pure);
    if yolo {
        eprintln!("hya: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
    }
    state = state.with_permission_requests(asks);
    spawn_provider_catalog_refresh(
        Arc::clone(&engine),
        state.catalog_updates_sender(),
        pending_discovery,
    );
    // Optional gRPC listener: HYA_GRPC_BIND=host:port serves the same
    // hya.v1 contract over tonic next to the HTTP surface.
    if let Some(grpc_bind) = std::env::var("HYA_GRPC_BIND")
        .ok()
        .filter(|value| !value.is_empty())
    {
        let grpc_state = state.clone();
        tokio::spawn(async move {
            if let Ok(listener) = tokio::net::TcpListener::bind(&grpc_bind).await {
                let addr = listener
                    .local_addr()
                    .map_or_else(|_| grpc_bind.clone(), |addr| addr.to_string());
                println!("hya grpc listening on http://{addr}");
                let grpc = hya_server::V1Grpc::new(grpc_state);
                use hya_api::v1 as pbv1;
                let server = tonic::transport::Server::builder()
                    .add_service(pbv1::process_server::ProcessServer::new(grpc.clone()))
                    .add_service(pbv1::catalog_server::CatalogServer::new(grpc.clone()))
                    .add_service(pbv1::agent_models_server::AgentModelsServer::new(
                        grpc.clone(),
                    ))
                    .add_service(pbv1::auth_server::AuthServer::new(grpc.clone()))
                    .add_service(pbv1::session_server::SessionServer::new(grpc.clone()))
                    .add_service(pbv1::turn_server::TurnServer::new(grpc.clone()))
                    .add_service(pbv1::messages_server::MessagesServer::new(grpc.clone()))
                    .add_service(pbv1::events_server::EventsServer::new(grpc.clone()))
                    .add_service(pbv1::interactions_server::InteractionsServer::new(
                        grpc.clone(),
                    ))
                    .add_service(pbv1::workflow_server::WorkflowServer::new(grpc.clone()))
                    .add_service(pbv1::files_server::FilesServer::new(grpc.clone()))
                    .add_service(pbv1::project_server::ProjectServer::new(grpc.clone()))
                    .add_service(pbv1::worktrees_server::WorktreesServer::new(grpc.clone()))
                    .add_service(pbv1::mcp_server::McpServer::new(grpc.clone()))
                    .add_service(pbv1::pty_server::PtyServer::new(grpc.clone()))
                    .add_service(pbv1::logs_server::LogsServer::new(grpc.clone()))
                    .add_service(pbv1::bundle_api_server::BundleApiServer::new(grpc.clone()))
                    .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
                let _ = server.await;
            }
        });
    }
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
    // On a stop signal, drain first: every in-flight turn in every session
    // (roots and members) is cancelled with cause `shutdown` and closes its
    // messages within the drain deadline, members go terminal, and new turns
    // are refused — so open turn requests finish and the HTTP server can
    // complete its graceful shutdown.
    let supervisor = built.resident_supervisor();
    let serve_result = axum::serve(listener, server_router(state))
        .with_graceful_shutdown(async move {
            wait_for_termination(terminate).await;
            supervisor
                .drain(hya_proto::FinishCause::Shutdown, hya_core::DRAIN_DEADLINE)
                .await;
        })
        .await
        .context("serve http");
    let shutdown_result = built.shutdown().await.context("shutdown spawn supervisor");
    serve_result.and(shutdown_result)
}

fn spawn_provider_catalog_refresh(
    engine: Arc<hya_core::SessionEngine>,
    catalog_updates: tokio::sync::broadcast::Sender<serde_json::Value>,
    pending: Vec<hya_app::config::PendingCatalogDiscovery>,
) {
    if pending.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let snapshot = engine.provider_catalog_snapshot();
        let router = engine.provider_router();
        match hya_app::config::refresh_pending_catalogs(pending, snapshot.as_ref(), router.as_ref())
            .await
        {
            Ok((router, catalog)) => {
                engine.publish_provider_catalog(Arc::new(router), catalog);
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
