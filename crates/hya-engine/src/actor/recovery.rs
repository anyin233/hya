//! Startup fencing and staging of resident actors. Starting their work remains explicit.

use super::ResidentSupervisor;
use crate::{AgentSpec, CoreError, SessionEngine};
use hya_proto::{OwnerRunId, SubagentMode};
use hya_store::{RecoveredActorClaim, SessionStore};
use std::path::PathBuf;
use std::sync::Arc;

/// Failure while reconstructing a resident, with its startup phase retained.
#[derive(Debug, thiserror::Error)]
#[error("{context}")]
pub struct ResidentRecoveryError {
    context: &'static str,
    #[source]
    source: Option<CoreError>,
}

impl ResidentRecoveryError {
    fn new(context: &'static str, source: impl Into<CoreError>) -> Self {
        Self {
            context,
            source: Some(source.into()),
        }
    }
}

/// Fence active actors before building a runtime or admitting new work.
pub async fn recover_resident_claims(
    store: &SessionStore,
    owner_run_id: OwnerRunId,
) -> Result<Vec<RecoveredActorClaim>, ResidentRecoveryError> {
    let mut recovered_claims = Vec::new();
    for actor_id in store.active_actor_ids().await.map_err(|error| {
        ResidentRecoveryError::new("list resident actors before startup recovery", error)
    })? {
        recovered_claims.push(store.recover_claim(actor_id, owner_run_id).await.map_err(
            |error| {
                ResidentRecoveryError::new("fence resident actor before startup recovery", error)
            },
        )?);
    }
    Ok(recovered_claims)
}

impl ResidentSupervisor {
    /// Start the listener and stage reconstructed actors without starting episodes.
    /// Admit restore prompts before calling `start_recovered`.
    pub async fn start_with_recovery(
        engine: Arc<SessionEngine>,
        owner_run_id: OwnerRunId,
        base: &AgentSpec,
        recovered_claims: Vec<RecoveredActorClaim>,
    ) -> Result<Arc<Self>, ResidentRecoveryError> {
        let resident_supervisor = Self::start_with_owner(engine.clone(), owner_run_id);
        for recovered in recovered_claims {
            let actor_id = recovered.claim.actor_id;
            let (root, _) = engine.session_lineage(actor_id).await.map_err(|error| {
                ResidentRecoveryError::new("resolve recovered resident root", error)
            })?;
            let root_projection = engine.read_projection_shared(root).await.map_err(|error| {
                ResidentRecoveryError::new("replay recovered resident roster", error)
            })?;
            let entry = root_projection
                .team
                .roster
                .values()
                .find(|entry| entry.session == actor_id && entry.mode == SubagentMode::Resident)
                .cloned()
                .ok_or_else(|| ResidentRecoveryError {
                    context: "active resident claim has no durable roster entry",
                    source: None,
                })?;
            let report = engine
                .recover_resident_actor(&recovered, root, &entry.handle)
                .await
                .map_err(|error| {
                    ResidentRecoveryError::new("terminalize recovered resident work", error)
                })?;
            let actor_projection =
                engine
                    .read_projection_shared(actor_id)
                    .await
                    .map_err(|error| {
                        ResidentRecoveryError::new("replay recovered resident session", error)
                    })?;
            let workdir = actor_projection
                .session
                .workdir
                .as_ref()
                .map(PathBuf::from)
                .ok_or_else(|| ResidentRecoveryError {
                    context: "recovered resident session has no workdir",
                    source: None,
                })?;
            // Resume is not a new spawn: exact catalog lookup only (no can_spawn,
            // no legacy general/base synthesis). Missing or inline-only identity fails.
            let recorded = actor_projection
                .session
                .agent
                .as_ref()
                .unwrap_or(&entry.agent_type);
            // The resident works in its session's workdir, not the process's.
            let base = AgentSpec {
                workdir: workdir.clone(),
                ..base.clone()
            };
            let recovered_binding = engine
                .bind_session_runtime(actor_id, &workdir)
                .await
                .map_err(|error| {
                    ResidentRecoveryError::new(
                        "bind recovered resident Session model configuration",
                        error,
                    )
                })?;
            let (recovered_agent, recovered_runtime) = engine
                .resolve_resident_runtime(recovered_binding, &base, recorded)
                .map_err(|error| {
                    ResidentRecoveryError::new(
                        "resolve recovered resident capabilities from current catalog",
                        error,
                    )
                })?;
            resident_supervisor.defer_recovered_resident(
                root,
                entry.handle,
                recovered_agent,
                recovered_runtime,
                recovered,
                report.work,
            );
        }
        Ok(resident_supervisor)
    }
}
