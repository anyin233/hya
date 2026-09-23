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
                    channel_policy,
                    reply,
                } => {
                    let result = submit_report_for_session(
                        &engine,
                        &supervisor,
                        session,
                        outcome,
                        report,
                        channel_policy,
                    )
                    .await
                    .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                LifecycleRequest::Archive {
                    session,
                    target,
                    reason,
                    reply,
                } => {
                    let result = supervisor
                        .archive_member(session, &target, &reason)
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
    channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
) -> Result<(), crate::CoreError> {
    let (root, _) = engine.session_lineage(session).await?;
    let handle = engine.resolve_handle(root, session).await?;
    if !channel_policy.is_some_and(|policy| policy.dm_child & (1 << 1) != 0) {
        return Err(crate::CoreError::Invalid(
            "channel policy denies report for this agent".to_string(),
        ));
    }
    supervisor
        .submit_report(root, &handle, outcome, report)
        .await
}
