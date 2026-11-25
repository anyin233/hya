//! Startup fencing and staging of resident actors. Starting their work remains explicit.

use super::{
    ActivationCompletion, Episode, EpisodeInit, RecoveredResident, ResidentRecovery,
    ResidentRecoveryReport, ResidentSupervisor, ResolvedResidentRuntime, RootDriverActivity,
    SlotStatus, TeamEvent, WorkflowActivation, await_episode,
};
use crate::ChannelPolicy;
use crate::{AgentSpec, CoreError, SessionEngine};
use hya_proto::{ActorClaim, OwnerRunId, SessionId, SubagentMode};
use hya_store::{RecoveredActorClaim, SessionStore};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Notify, oneshot, watch};

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

impl SessionEngine {
    /// Recover durable resident state after restart and report next work.
    ///
    /// # Errors
    /// Returns store/validation failures.
    pub async fn recover_resident_actor(
        &self,
        recovered: &hya_store::RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<ResidentRecoveryReport, CoreError> {
        let work = self
            .recover_resident_actor_durable(recovered, root, handle)
            .await?;
        Ok(ResidentRecoveryReport {
            work: map_recovered_resident_work(work),
        })
    }
}

fn map_recovered_resident_work(work: hya_store::RecoveredResidentWork) -> ResidentRecovery {
    match work {
        hya_store::RecoveredResidentWork::Idle => ResidentRecovery::Idle,
        hya_store::RecoveredResidentWork::Queued { inbox_cursor } => {
            ResidentRecovery::Queued { inbox_cursor }
        }
        hya_store::RecoveredResidentWork::AbortedRunning {
            inbox_cursor,
            queued_after,
        } => ResidentRecovery::AbortedRunning {
            inbox_cursor,
            queued_after,
        },
    }
}

impl ResidentSupervisor {
    /// Prepare outside the claim-write lock, then commit and install a revived
    /// episode under the same serialization as every archive/Takeover decision.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn revive_episode(
        &self,
        root: SessionId,
        parent: &str,
        target: &str,
        channel: String,
        kind: hya_proto::MailKind,
        body: String,
        parent_claim: Option<&ActorClaim>,
        policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
    ) -> Result<usize, CoreError> {
        self.revive_episode_with_activation(
            root,
            parent,
            target,
            channel,
            kind,
            body,
            parent_claim,
            policy,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn revive_episode_with_activation(
        &self,
        root: SessionId,
        parent: &str,
        target: &str,
        channel: String,
        kind: hya_proto::MailKind,
        body: String,
        parent_claim: Option<&ActorClaim>,
        policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
        activation: Option<WorkflowActivation>,
    ) -> Result<usize, CoreError> {
        let projection = self.engine.read_projection_shared(root).await?;
        // A concurrent sender may already have installed the new episode.
        let child = projection
            .team
            .roster
            .get(target)
            .map(|entry| entry.session)
            .or_else(|| {
                projection
                    .team
                    .archived
                    .get(target)
                    .map(|entry| entry.session)
            })
            .ok_or_else(|| CoreError::Invalid(format!("resident not found: {target}")))?;
        let parent_session = projection
            .team
            .roster
            .get(parent)
            .map_or(root, |entry| entry.session);
        drop(projection);
        let (agent, (binding, agents, resources, sidecar_factory)) =
            if let Some(activation) = &activation {
                activation.resolved.clone()
            } else {
                let child_projection = self.engine.read_projection_shared(child).await?;
                let stable =
                    child_projection.session.agent.clone().ok_or_else(|| {
                        CoreError::Invalid("resident has no recorded agent".into())
                    })?;
                let workdir = child_projection
                    .session
                    .workdir
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| CoreError::Invalid(format!("session not found: {child}")))?;
                let base = AgentSpec {
                    name: stable.clone(),
                    model: child_projection
                        .session
                        .model
                        .clone()
                        .ok_or_else(|| CoreError::Invalid("resident has no model".into()))?,
                    system_prompt: self.engine.base_system_prompt().into(),
                    workdir,
                    reasoning: None,
                };
                drop(child_projection);
                let binding = self
                    .engine
                    .bind_session_runtime(child, &base.workdir)
                    .await?;
                self.engine
                    .resolve_resident_runtime(binding, &base, &stable)?
            };
        let channel_policy = ChannelPolicy::from_binding(&binding)
            .map_err(CoreError::bundle)?
            .snapshot_for(agent.name.as_str());
        let mut team = self.team_for(root).await?;
        #[cfg(test)]
        {
            let pause = self
                .registration_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let mut write = self.claim_writes.lock().await;
        let live = loop {
            let current = self.engine.read_projection_shared(root).await?;
            // A live roster wins: never wait for a concurrent revival's episode.
            if current.team.roster.contains_key(target) {
                break true;
            }
            let previous = self.team_with_resident(child).and_then(|previous| {
                previous
                    .lock()
                    .residents
                    .get(&child)
                    .map(|slot| slot.done.subscribe())
            });
            let Some(done) = previous else {
                break false;
            };
            drop(current);
            drop(write);
            let _ = await_episode(done).await;
            team = self.team_for(root).await?;
            write = self.claim_writes.lock().await;
        };
        // Reserve the existing turn seam before checking idle: unrelated mail
        // cannot start work until guidance, route and completion are installed.
        let activation_lease = activation
            .as_ref()
            .map(|_| self.engine.try_begin_turn(child))
            .transpose()?;
        if activation.is_some() {
            let state = team.lock();
            if let Some(slot) = state.residents.get(&child) {
                if slot.status != SlotStatus::Idle
                    || slot.has_work()
                    || slot.activation.is_some()
                    || slot.ending.is_some()
                    || slot.report.is_some()
                    || slot.user_stopped
                {
                    return Err(CoreError::Invalid(
                        "resident Workflow activation requires an idle episode".into(),
                    ));
                }
            } else if live {
                return Err(CoreError::Invalid(
                    "resident session is not registered".into(),
                ));
            }
        }
        let (claim, envelopes, recipients) = self
            .engine
            .store()
            .revive_episode(
                root,
                self.owner_run_id,
                self.capacity(),
                parent_claim,
                child,
                parent.into(),
                channel,
                kind,
                body,
                hya_provider::COMPACT_CONTEXT_MARKER,
                policy,
            )
            .await?;
        let activation = if let Some(activation) = activation {
            let projection = self.engine.read_projection_shared(root).await?;
            let boundary = projection.team.inboxes[target].len();
            Some((activation, boundary))
        } else {
            None
        };
        let Some((claim, cursor)) = claim else {
            if let Some((activation, boundary)) = activation {
                let mut state = team.lock();
                let slot = state.residents.get_mut(&child).ok_or_else(|| {
                    CoreError::Invalid("resident episode disappeared during activation".into())
                })?;
                slot.guidance = Some(activation.guidance);
                slot.route = activation.route;
                slot.activation = Some(ActivationCompletion {
                    boundary,
                    done: activation.done,
                });
                slot.pending = true;
            }
            for envelope in envelopes {
                self.engine.publish_envelope(envelope);
            }
            return Ok(recipients);
        };
        let activated = activation.is_some();
        let (guidance, route, completion) = match activation {
            Some((activation, boundary)) => (
                Some(activation.guidance),
                activation.route,
                Some(ActivationCompletion {
                    boundary,
                    done: activation.done,
                }),
            ),
            None => (None, None, None),
        };
        self.engine
            .update_session_channel_policy(child, channel_policy);
        let (ready, initialized) = oneshot::channel();
        let (publication, published) = oneshot::channel();
        let (child_envelopes, parent_envelopes): (Vec<_>, Vec<_>) = envelopes
            .into_iter()
            .partition(|envelope| envelope.event.session() == Some(child));
        let init = EpisodeInit {
            binding: binding.clone(),
            agent: agent.name.clone(),
            parent: parent_session,
            envelopes: child_envelopes,
            published,
            ready,
        };
        team.install_episode(
            child,
            Episode {
                handle: target.into(),
                agent,
                binding,
                agents: Some(agents),
                resources: Some(resources),
                guidance,
                route,
                activation: completion,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending: true,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: Some(claim),
                cancel: team.cancel.child_token(),
                ending: None,
                done: watch::channel(None).0,
                cursor: usize::try_from(cursor).unwrap_or(usize::MAX),
                last_activity: None,
                last_heartbeat_emit: None,
                notify: Arc::new(Notify::new()),
            },
            Some(init),
        );
        for envelope in parent_envelopes {
            self.engine.publish_envelope(envelope);
        }
        let _ = publication.send(());
        drop(write);
        drop(activation_lease);
        if activated {
            return Ok(recipients);
        }
        initialized
            .await
            .map_err(|_| CoreError::Invalid("resident initialization task exited".into()))??;
        #[cfg(test)]
        {
            let pause = self
                .revive_ready_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        Ok(recipients)
    }

    /// Stage a fenced actor without creating a team or starting any work.
    pub fn defer_recovered_resident(
        &self,
        root: SessionId,
        handle: String,
        agent: AgentSpec,
        runtime: ResolvedResidentRuntime,
        claim: hya_store::RecoveredActorClaim,
        disposition: ResidentRecovery,
    ) {
        self.recovered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RecoveredResident {
                root,
                handle,
                agent,
                runtime,
                claim,
                disposition,
            });
    }

    /// Start a headless runtime's recovered actors under external root ownership.
    /// Keep these guards until the headless command has drained its runtime.
    pub async fn start_recovered_headless(
        &self,
        session: SessionId,
    ) -> Result<Vec<RootDriverActivity>, CoreError> {
        let mut roots = self
            .recovered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|member| member.root)
            .collect::<HashSet<_>>();
        roots.insert(self.engine.session_lineage(session).await?.0);
        let mut drivers = Vec::with_capacity(roots.len());
        for root in roots {
            drivers.push(self.root_driver_activity(root).await?);
        }
        self.start_recovered().await?;
        Ok(drivers)
    }

    /// Install recovered episodes after the server has admitted restore prompts.
    /// Repeated calls have no effects once the staged actors have been installed.
    pub async fn start_recovered(&self) -> Result<usize, CoreError> {
        let recovered = std::mem::take(
            &mut *self
                .recovered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let mut count = 0;
        for member in recovered {
            match self
                .register_recovered_resident(
                    member.root,
                    member.handle,
                    member.agent,
                    member.runtime,
                    member.claim,
                    member.disposition,
                )
                .await
            {
                Ok(()) => count += 1,
                // Restore tools may end or replace a staged claim before installation.
                Err(CoreError::Store(hya_store::StoreError::StaleActorClaim { .. })) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(count)
    }

    /// Re-register a recovered resident into the live supervisor map.
    pub async fn register_recovered_resident(
        &self,
        root: SessionId,
        handle: String,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        recovered: hya_store::RecoveredActorClaim,
        disposition: ResidentRecovery,
    ) -> Result<(), CoreError> {
        let (binding, agents, resources, sidecar_factory) = resolved;
        let team = self.team_for(root).await?;
        // Transfer liveness to recovery before a stale claim can drop registration.
        team.enqueue(TeamEvent::Recover);
        let _write = self.claim_writes.lock().await;
        self.engine
            .store()
            .validate_actor_claim(&recovered.claim)
            .await?;
        let projection = self.engine.read_projection_shared(root).await?;
        let cursor = projection
            .team
            .roster
            .get(&handle)
            .filter(|entry| entry.session == recovered.claim.actor_id)
            .map_or(0, |entry| entry.resident_cursor);
        let pending = matches!(
            disposition,
            ResidentRecovery::Queued { .. }
                | ResidentRecovery::AbortedRunning {
                    queued_after: true,
                    ..
                }
        ) || projection
            .team
            .unread_incoming_mail(&handle, cursor)
            .next()
            .is_some();
        if team
            .lock()
            .residents
            .contains_key(&recovered.claim.actor_id)
        {
            return Err(CoreError::Invalid("resident already has an episode".into()));
        }
        let notify = Arc::new(Notify::new());
        team.install_episode(
            recovered.claim.actor_id,
            Episode {
                handle,
                agent,
                binding,
                agents: Some(agents),
                resources: Some(resources),
                // Ephemeral guidance is not durable; recovery invents nothing.
                guidance: None,
                route: None,
                activation: None,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: Some(recovered.claim),
                cancel: team.cancel.child_token(),
                ending: None,
                done: watch::channel(None).0,
                cursor: usize::try_from(cursor).unwrap_or(usize::MAX),
                last_activity: None,
                last_heartbeat_emit: None,
                notify: notify.clone(),
            },
            None,
        );
        Ok(())
    }
}
