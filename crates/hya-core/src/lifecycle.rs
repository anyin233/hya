//! Lifecycle service loop (ADR-0015): drains `LifecycleRequest`s from the
//! tool plane into the resident supervisor. Typed core errors are flattened
//! to strings on the reply channel, mirroring the mailbox service.

use std::sync::Arc;

use hya_proto::ReportOutcome;
use hya_tool::LifecycleRequest;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::engine::SessionEngine;
use crate::resident::ResidentSupervisor;

/// Run the lifecycle service until the plane (and all its clones) are dropped.
pub async fn run_lifecycle_service(
    engine: Arc<SessionEngine>,
    supervisor: Arc<ResidentSupervisor>,
    mut rx: UnboundedReceiver<LifecycleRequest>,
) {
    while let Some(request) = rx.recv().await {
        let engine = engine.clone();
        let supervisor = supervisor.clone();
        // One task per request so a slow gate check cannot head-of-line block
        // the others.
        tokio::spawn(async move {
            match request {
                LifecycleRequest::Report {
                    session,
                    outcome,
                    report,
                    reply,
                } => {
                    let result =
                        submit_report_for_session(&engine, &supervisor, session, outcome, report)
                            .await
                            .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                LifecycleRequest::Kill {
                    session,
                    handle,
                    reason,
                    reply,
                } => {
                    let result = kill_for_session(&engine, &supervisor, session, &handle, &reason)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
            }
        });
    }
}

/// Resolve the acting session to its team handle, then submit the report.
async fn submit_report_for_session(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    session: hya_proto::SessionId,
    outcome: ReportOutcome,
    report: String,
) -> Result<(), crate::CoreError> {
    let (root, _) = engine.session_lineage(session).await?;
    let handle = engine.resolve_handle(root, session).await?;
    supervisor
        .submit_report(root, &handle, outcome, report)
        .await
}

/// Resolve the killing session to its team root, then force-kill the child.
async fn kill_for_session(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    session: hya_proto::SessionId,
    handle: &str,
    reason: &str,
) -> Result<(), crate::CoreError> {
    let (root, _) = engine.session_lineage(session).await?;
    supervisor.kill_and_archive(root, handle, reason).await
}
