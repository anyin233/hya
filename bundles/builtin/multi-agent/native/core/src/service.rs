//! Managed ABI v2 [`NativeService`] owning multi-agent supervision, actor
//! claim leases, member inspection, subagent archive/steer control, and
//! subagent wait evaluation over [`HostRuntime`].
//!
//! All host interactions flow exclusively through `Arc<dyn HostRuntime>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hya_proto::{
    AgentName, Event, MemberId, MemberRunStatus, MessageId, OwnerRunId, Projection, Role,
    RosterStatus, SessionId, SubagentMode, scope,
};
use hya_runtime_abi::{HostError, HostRuntime, NativeService, ServiceHealth};
use hya_tool::extension::{
    ArchivedEntry, BUNDLE_MULTI_AGENT, ExtensionEvent, MemberProjection, RosterEntry,
    handoff_projection, members_projection, team_projection,
};
use hya_tool::{
    ArchiveReason, ArchiveReceipt, MailReceipt, WaitMember, WaitMemberState, WaitMode, WaitOutcome,
    WaitSpec, WaitWake,
};

use crate::claims::{
    FencedActorLease, MULTI_AGENT_BUNDLE_ID, acquire_actor_claim, recover_stale_actor_claims,
    release_actor_claim,
};
use crate::governor::{SubagentGovernor, SubagentLimits};
use crate::handle_naming::HandleNamer;
use crate::handoff::{TerminalHandoff, degraded_terminal_handoff};

/// Canonical service id registered with ABI v2.
pub const MULTI_AGENT_SERVICE_ID: &str = "multi-agent";

/// Prefix placed on user-originated subagent steer messages.
pub const USER_STEER_PREFIX: &str = "[user] ";

const MAX_LINEAGE_DEPTH: usize = 64;
const WAIT_RECHECK: Duration = Duration::from_secs(5);

/// One resolved `archive`/`wait` target inside a team.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberTarget {
    /// Canonical handle (e.g. `main/hya-task-exusiai`).
    pub handle: String,
    /// Target member's session id.
    pub session: SessionId,
    /// Whether the member is currently on the live roster (`false`: archived).
    pub live: bool,
}

/// Snapshot returned by `GET /members` for a session and its team.
#[derive(Clone, Debug, PartialEq)]
pub struct MembersSnapshot {
    /// Queried session id.
    pub session: SessionId,
    /// Owning team root session id.
    pub root: SessionId,
    /// Spawned member rows recorded on `session`'s log.
    pub members: Vec<MemberProjection>,
    /// Live team roster entries on `root`.
    pub roster: Vec<RosterEntry>,
    /// Archived team entries on `root`.
    pub archived: Vec<ArchivedEntry>,
}

struct WaitStallRecord {
    last_answer: Option<MessageId>,
}

/// Managed multi-agent orchestration service backed solely by [`HostRuntime`].
pub struct MultiAgentService {
    runtime: RwLock<Option<Arc<dyn HostRuntime>>>,
    governor: RwLock<SubagentGovernor>,
    handle_namer: Arc<HandleNamer>,
    active_leases: Mutex<HashMap<SessionId, FencedActorLease>>,
    wait_stalls: Mutex<HashMap<(SessionId, SessionId), WaitStallRecord>>,
}

impl Default for MultiAgentService {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiAgentService {
    /// Construct an unstarted service awaiting [`NativeService::start`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime: RwLock::new(None),
            governor: RwLock::new(SubagentGovernor::new(SubagentLimits::default())),
            handle_namer: Arc::new(HandleNamer::default()),
            active_leases: Mutex::new(HashMap::new()),
            wait_stalls: Mutex::new(HashMap::new()),
        }
    }

    /// Construct a service pre-bound to `runtime` (useful for tests and
    /// direct in-process embedding).
    #[must_use]
    pub fn with_runtime(runtime: Arc<dyn HostRuntime>) -> Self {
        Self {
            runtime: RwLock::new(Some(runtime)),
            governor: RwLock::new(SubagentGovernor::new(SubagentLimits::default())),
            handle_namer: Arc::new(HandleNamer::default()),
            active_leases: Mutex::new(HashMap::new()),
            wait_stalls: Mutex::new(HashMap::new()),
        }
    }

    /// Return whether the service has been started with a [`HostRuntime`].
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// Return the currently bound [`HostRuntime`], or [`HostError::Unavailable`].
    pub fn runtime(&self) -> Result<Arc<dyn HostRuntime>, HostError> {
        self.runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                HostError::Unavailable("hya/multi-agent service is not started".to_string())
            })
    }

    /// Return a clone of the service's [`SubagentGovernor`].
    #[must_use]
    pub fn governor(&self) -> SubagentGovernor {
        self.governor
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Return the shared [`HandleNamer`].
    #[must_use]
    pub fn handle_namer(&self) -> Arc<HandleNamer> {
        Arc::clone(&self.handle_namer)
    }

    /// Clear tracked in-memory stall records involving `session`.
    pub fn clear_session_state(&self, session: SessionId) {
        self.wait_stalls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|&(waiter, target), _| waiter != session && target != session);
    }

    /// Resolve `(team_root, depth)` for `session` by walking parent links.
    pub async fn session_lineage(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<(SessionId, u32), HostError> {
        let mut current = session;
        let mut depth: u32 = 0;
        for _ in 0..MAX_LINEAGE_DEPTH {
            let Some(meta) = runtime.session_store().session_metadata(current).await? else {
                break;
            };
            match meta.parent {
                Some(parent) if parent != current => {
                    current = parent;
                    depth = depth.saturating_add(1);
                }
                _ => break,
            }
        }
        Ok((current, depth))
    }

    /// Resolve the team root session id for `session`.
    pub async fn team_root(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<SessionId, HostError> {
        Ok(Self::session_lineage(runtime, session).await?.0)
    }

    /// Read folded subagent members, live team roster, and archived entries for `session`.
    pub async fn list_members(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<MembersSnapshot, HostError> {
        if !runtime.session_store().session_exists(session).await? {
            return Err(HostError::NotFound(format!("session not found: {session}")));
        }
        let root = Self::team_root(runtime, session).await?;
        let session_proj = runtime.session_store().read_projection(session).await?;
        let members = members_projection(&session_proj);
        let root_proj = if root == session {
            session_proj
        } else {
            runtime.session_store().read_projection(root).await?
        };
        let team = team_projection(&root_proj);
        Ok(MembersSnapshot {
            session,
            root,
            members,
            roster: team.roster.into_values().collect(),
            archived: team.archived.into_values().collect(),
        })
    }

    /// Archive `child` on the user's behalf via [`HostTurnRunner::archive_subagent`].
    pub async fn archive_subagent(
        runtime: &dyn HostRuntime,
        child: SessionId,
    ) -> Result<ArchiveReceipt, HostError> {
        runtime.turn_runner().archive_subagent(child).await
    }

    /// Steer `child` with a user message via [`HostTurnRunner::steer_subagent`].
    pub async fn steer_subagent(
        runtime: &dyn HostRuntime,
        child: SessionId,
        text: &str,
    ) -> Result<MailReceipt, HostError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(HostError::InvalidInput(
                "a steer message needs text".to_string(),
            ));
        }
        runtime.turn_runner().steer_subagent(child, trimmed).await
    }

    /// Acquire a fenced actor lease in `bundle_claim` and record
    /// `AgentRegistered` + `MemberSpawned` extension events for a resident actor.
    #[allow(clippy::too_many_arguments)]
    pub async fn register_resident_actor(
        &self,
        runtime: &dyn HostRuntime,
        root: SessionId,
        parent: SessionId,
        parent_handle: &str,
        actor_session: SessionId,
        leaf: &str,
        agent_type: AgentName,
        description: String,
        depth: u32,
        directive: String,
    ) -> Result<(FencedActorLease, MemberId), HostError> {
        let owner = runtime
            .session_store()
            .runtime_owner_id()
            .unwrap_or_default();
        let fenced = acquire_actor_claim(runtime, root, actor_session, owner, 0).await?;
        let member = MemberId::new();
        let reg_event = ExtensionEvent::AgentRegistered {
            session: root,
            agent_session: actor_session,
            handle: leaf.to_string(),
            parent: Some(parent_handle.to_string()),
            agent_type: agent_type.clone(),
            mode: SubagentMode::Resident,
        };
        let spawned_event = ExtensionEvent::MemberSpawned {
            session: parent,
            member,
            child: Some(actor_session),
            subagent_type: agent_type,
            description,
            depth,
            directive,
            tool_call: None,
        };
        let env1 = runtime
            .session_store()
            .append_extension_event(
                root,
                BUNDLE_MULTI_AGENT,
                reg_event.kind(),
                extension_payload(&reg_event),
                Some(fenced.claim),
            )
            .await?;
        let _ = runtime.event_bus().publish(env1);
        let env2 = runtime
            .session_store()
            .append_extension_event(
                parent,
                BUNDLE_MULTI_AGENT,
                spawned_event.kind(),
                extension_payload(&spawned_event),
                None,
            )
            .await?;
        let _ = runtime.event_bus().publish(env2);
        self.active_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(actor_session, fenced.clone());
        Ok((fenced, member))
    }

    /// Record `ResidentWorkStarted` fenced by `fenced.claim`.
    pub async fn record_work_started(
        runtime: &dyn HostRuntime,
        fenced: &FencedActorLease,
        handle: &str,
        inbox_through: u64,
    ) -> Result<(), HostError> {
        let event = ExtensionEvent::ResidentWorkStarted {
            session: fenced.root,
            actor_session: fenced.claim.actor_id,
            handle: handle.to_string(),
            epoch: fenced.claim.epoch,
            inbox_through,
        };
        let env = runtime
            .session_store()
            .append_extension_event(
                fenced.root,
                BUNDLE_MULTI_AGENT,
                event.kind(),
                extension_payload(&event),
                Some(fenced.claim),
            )
            .await?;
        let _ = runtime.event_bus().publish(env);
        Ok(())
    }

    /// Record `ResidentWorkFinished` fenced by `fenced.claim`.
    pub async fn record_work_finished(
        runtime: &dyn HostRuntime,
        fenced: &FencedActorLease,
        handle: &str,
        inbox_through: u64,
    ) -> Result<(), HostError> {
        let event = ExtensionEvent::ResidentWorkFinished {
            session: fenced.root,
            handle: handle.to_string(),
            epoch: fenced.claim.epoch,
            inbox_through,
        };
        let env = runtime
            .session_store()
            .append_extension_event(
                fenced.root,
                BUNDLE_MULTI_AGENT,
                event.kind(),
                extension_payload(&event),
                Some(fenced.claim),
            )
            .await?;
        let _ = runtime.event_bus().publish(env);
        Ok(())
    }

    /// Commit `HandoffCommitted` + `AgentArchived` and release the actor's
    /// `bundle_claim` lease.
    pub async fn commit_actor_archive(
        &self,
        runtime: &dyn HostRuntime,
        fenced: &FencedActorLease,
        handle: &str,
        reason: ArchiveReason,
        handoff: Option<TerminalHandoff>,
    ) -> Result<TerminalHandoff, HostError> {
        let handoff = match handoff {
            Some(h) => h,
            None => degraded_terminal_handoff(runtime, fenced.claim.actor_id).await,
        };
        let handoff_event = ExtensionEvent::HandoffCommitted {
            session: fenced.claim.actor_id,
            handle: handle.to_string(),
            generation: handoff.generation,
            doc: handoff.doc.clone(),
            degraded: handoff.degraded,
        };
        let env_h = runtime
            .session_store()
            .append_extension_event(
                fenced.claim.actor_id,
                BUNDLE_MULTI_AGENT,
                handoff_event.kind(),
                extension_payload(&handoff_event),
                Some(fenced.claim),
            )
            .await?;
        let _ = runtime.event_bus().publish(env_h);

        let archived_event = ExtensionEvent::AgentArchived {
            session: fenced.root,
            handle: handle.to_string(),
            child: fenced.claim.actor_id,
            reason,
        };
        let env_a = runtime
            .session_store()
            .append_extension_event(
                fenced.root,
                BUNDLE_MULTI_AGENT,
                archived_event.kind(),
                extension_payload(&archived_event),
                Some(fenced.claim),
            )
            .await?;
        let _ = runtime.event_bus().publish(env_a);
        release_actor_claim(runtime, fenced).await?;
        self.active_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&fenced.claim.actor_id);
        Ok(handoff)
    }

    /// Recover stale actor claims left by a prior process run via `HostBundleStorage`.
    pub async fn recover_claims(
        &self,
        runtime: &dyn HostRuntime,
        owner: OwnerRunId,
    ) -> Result<usize, HostError> {
        let recovered = recover_stale_actor_claims(runtime, owner).await?;
        let count = recovered.len();
        let mut leases = self
            .active_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (lease, _) in recovered {
            leases.insert(lease.claim.actor_id, lease);
        }
        Ok(count)
    }

    /// Evaluate `wait` for `caller` exclusively over [`HostRuntime`].
    pub async fn wait_for_members(
        &self,
        runtime: &dyn HostRuntime,
        caller: SessionId,
        spec: WaitSpec,
    ) -> Result<WaitOutcome, HostError> {
        let started = Instant::now();
        let mut bus = runtime.event_bus().subscribe()?;
        let root = Self::team_root(runtime, caller).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        let caller_path = team
            .roster
            .values()
            .find(|entry| entry.session == caller)
            .map(|entry| entry.handle.clone())
            .or_else(|| (caller == root).then(|| scope::ROOT_HANDLE.to_string()))
            .ok_or_else(|| {
                HostError::InvalidInput(
                    "caller has no team handle; it must be a team member to wait".to_string(),
                )
            })?;
        let resolved = resolve_wait_targets(&projection, root, &caller_path, &spec.targets)?;
        let mut targets = Vec::with_capacity(resolved.len());
        for (handle, session) in resolved {
            let done_at_start = team
                .roster
                .get(&handle)
                .filter(|entry| entry.session == session)
                .is_none_or(|entry| is_terminal_roster_status(entry.status));
            let generation = handoff_generation(runtime, session).await?;
            targets.push((handle, session, done_at_start, generation));
        }

        let deadline = tokio::time::Instant::now() + spec.timeout;
        loop {
            let projection = runtime.session_store().read_projection(root).await?;
            let team = team_projection(&projection);
            let mut finished = Vec::new();
            let mut already = Vec::new();
            let mut running = Vec::new();
            let mut stalled_targets: Vec<(SessionId, Option<MessageId>)> = Vec::new();

            for (handle, session, done_at_start, start_gen) in &targets {
                let live = team
                    .roster
                    .get(handle)
                    .filter(|entry| entry.session == *session);
                if let Some(entry) = live.filter(|entry| !is_terminal_roster_status(entry.status)) {
                    let stopped = runtime
                        .turn_runner()
                        .is_member_stopped_by_user(root, handle);
                    let status = runtime.turn_runner().turn_status(*session).await.ok();
                    let busy = !entry.mode.is_resident()
                        || entry.status == RosterStatus::Busy
                        || matches!(status, Some(hya_runtime_abi::TurnStatus::Running));
                    let leads_live = team
                        .roster
                        .keys()
                        .any(|path| scope::parent_path(path) == Some(handle.as_str()));
                    let state = if stopped
                        && !matches!(status, Some(hya_runtime_abi::TurnStatus::Running))
                    {
                        WaitMemberState::StoppedByUser
                    } else if busy || leads_live {
                        WaitMemberState::Working
                    } else {
                        let last = last_assistant_message(runtime, *session).await?;
                        stalled_targets.push((*session, last));
                        WaitMemberState::Idle
                    };
                    running.push(WaitMember {
                        handle: handle.clone(),
                        session: *session,
                        state,
                        outcome: None,
                        report: None,
                        channel: None,
                    });
                    continue;
                }
                let reason = if live.is_some() {
                    None
                } else {
                    team.archived
                        .get(handle)
                        .filter(|entry| entry.session == *session)
                        .map(|entry| entry.reason)
                };
                let row = terminal_member_row(runtime, *session).await?;
                let (state, outcome, report) = match (reason, row) {
                    (
                        None | Some(ArchiveReason::Reported),
                        Some((MemberRunStatus::Done, summary)),
                    ) => (
                        WaitMemberState::Reported,
                        Some("done".to_string()),
                        Some(summary),
                    ),
                    (
                        None | Some(ArchiveReason::Reported),
                        Some((MemberRunStatus::Failed, summary)),
                    ) => (
                        WaitMemberState::Reported,
                        Some("failed".to_string()),
                        Some(summary),
                    ),
                    (Some(_), Some((MemberRunStatus::Failed, note))) => (
                        WaitMemberState::Archived,
                        Some("failed".to_string()),
                        Some(note),
                    ),
                    (_, Some((MemberRunStatus::Cancelled, note))) => (
                        WaitMemberState::Archived,
                        Some("cancelled".to_string()),
                        Some(note),
                    ),
                    _ => (WaitMemberState::Archived, None, None),
                };
                let member = WaitMember {
                    handle: handle.clone(),
                    session: *session,
                    state,
                    outcome,
                    report: report
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    channel: None,
                };
                let current_gen = handoff_generation(runtime, *session).await?;
                if !*done_at_start || current_gen > *start_gen {
                    finished.push(member);
                } else {
                    already.push(member);
                }
            }

            let only_stopped = running
                .iter()
                .all(|member| member.state == WaitMemberState::StoppedByUser);
            let members_done = match spec.mode {
                WaitMode::Any => !finished.is_empty(),
                WaitMode::All => !finished.is_empty() && only_stopped,
            };
            let new_stall = {
                let stalls = self
                    .wait_stalls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                stalled_targets.iter().any(|(target_session, last)| {
                    !stalls
                        .get(&(caller, *target_session))
                        .is_some_and(|rec| rec.last_answer == *last)
                })
            };
            let nothing_left = finished.is_empty() && only_stopped;
            let woke_by = if members_done {
                Some(WaitWake::Members)
            } else if new_stall {
                Some(WaitWake::Stalled)
            } else if nothing_left {
                Some(WaitWake::NothingToWaitFor)
            } else if tokio::time::Instant::now() >= deadline {
                Some(WaitWake::Timeout)
            } else {
                None
            };

            if let Some(woke_by) = woke_by {
                if !stalled_targets.is_empty() {
                    let mut stalls = self
                        .wait_stalls
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    for (target_session, last_answer) in stalled_targets {
                        stalls.insert((caller, target_session), WaitStallRecord { last_answer });
                    }
                }
                return Ok(WaitOutcome {
                    woke_by,
                    finished,
                    already_finished: already,
                    running,
                    mail: Vec::new(),
                    more_mail: false,
                    waited_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                });
            }

            let recheck = (tokio::time::Instant::now() + WAIT_RECHECK).min(deadline);
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(recheck) => break,
                    received = bus.recv() => match received {
                        Ok(envelope) if is_relevant_wait_event(&envelope.event, root, caller) => break,
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return Err(HostError::Unavailable("event bus closed during wait".into()));
                        }
                    }
                }
            }
        }
    }
}

#[async_trait]
impl NativeService for MultiAgentService {
    fn id(&self) -> &str {
        MULTI_AGENT_SERVICE_ID
    }

    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        let cfg = runtime
            .catalog_reader()
            .bundle_config(MULTI_AGENT_BUNDLE_ID)
            .await
            .unwrap_or(serde_json::Value::Null);
        let limits = parse_subagent_limits_from_config(&cfg);
        *self
            .governor
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = SubagentGovernor::new(limits);
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(runtime);
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        let runtime = self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let leases: Vec<FencedActorLease> = self
            .active_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, lease)| lease)
            .collect();
        if let Some(runtime) = runtime {
            for lease in leases {
                let _ = release_actor_claim(runtime.as_ref(), &lease).await;
            }
        }
        self.wait_stalls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        if self.is_started() {
            ServiceHealth::healthy("ready")
        } else {
            ServiceHealth::unhealthy("stopped")
        }
    }
}

/// Resolve a handle path, leaf, or session id against the team projection.
pub fn resolve_member_target(
    projection: &Projection,
    root: SessionId,
    caller_path: &str,
    raw: &str,
) -> Result<MemberTarget, HostError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(HostError::InvalidInput("target must not be empty".into()));
    }
    let team = team_projection(projection);
    if let Ok(session) = raw.parse::<SessionId>() {
        if session == root {
            return Ok(MemberTarget {
                handle: scope::ROOT_HANDLE.to_string(),
                session: root,
                live: true,
            });
        }
        if let Some(entry) = team.roster.values().find(|entry| entry.session == session) {
            return Ok(MemberTarget {
                handle: entry.handle.clone(),
                session,
                live: true,
            });
        }
        if let Some(entry) = team
            .archived
            .values()
            .find(|entry| entry.session == session)
        {
            return Ok(MemberTarget {
                handle: entry.handle.clone(),
                session,
                live: false,
            });
        }
        return Err(HostError::InvalidInput(format!(
            "session `{session}` is not a member of this team"
        )));
    }
    if raw == scope::ROOT_HANDLE {
        return Ok(MemberTarget {
            handle: scope::ROOT_HANDLE.to_string(),
            session: root,
            live: true,
        });
    }
    if let Some(entry) = team.roster.get(raw) {
        return Ok(MemberTarget {
            handle: entry.handle.clone(),
            session: entry.session,
            live: true,
        });
    }
    if let Some(entry) = team.archived.get(raw) {
        return Ok(MemberTarget {
            handle: entry.handle.clone(),
            session: entry.session,
            live: false,
        });
    }
    let direct = scope::join_path(caller_path, raw);
    if let Some(entry) = team.roster.get(&direct) {
        return Ok(MemberTarget {
            handle: entry.handle.clone(),
            session: entry.session,
            live: true,
        });
    }
    if let Some(entry) = team.archived.get(&direct) {
        return Ok(MemberTarget {
            handle: entry.handle.clone(),
            session: entry.session,
            live: false,
        });
    }
    let live_matches: Vec<_> = team
        .roster
        .values()
        .filter(|entry| scope::leaf(&entry.handle) == raw)
        .collect();
    if live_matches.len() == 1 {
        let entry = live_matches[0];
        return Ok(MemberTarget {
            handle: entry.handle.clone(),
            session: entry.session,
            live: true,
        });
    }
    if live_matches.len() > 1 {
        let handles = live_matches
            .iter()
            .map(|entry| entry.handle.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(HostError::InvalidInput(format!(
            "ambiguous target `{raw}`; use a full handle: {handles}"
        )));
    }
    let archived_matches: Vec<_> = team
        .archived
        .values()
        .filter(|entry| scope::leaf(&entry.handle) == raw)
        .collect();
    if archived_matches.len() == 1 {
        let entry = archived_matches[0];
        return Ok(MemberTarget {
            handle: entry.handle.clone(),
            session: entry.session,
            live: false,
        });
    }
    Err(HostError::InvalidInput(format!(
        "unknown team member `{raw}`"
    )))
}

fn resolve_wait_targets(
    projection: &Projection,
    root: SessionId,
    caller_path: &str,
    requested: &[String],
) -> Result<Vec<(String, SessionId)>, HostError> {
    if requested.is_empty() {
        return Ok(team_projection(projection)
            .roster
            .iter()
            .filter(|(path, _)| scope::parent_path(path) == Some(caller_path))
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect());
    }
    let prefix = format!("{caller_path}{}", scope::PATH_SEPARATOR);
    let mut targets: Vec<(String, SessionId)> = Vec::new();
    for raw in requested {
        let target = resolve_member_target(projection, root, caller_path, raw)?;
        if target.session == root || target.handle == scope::ROOT_HANDLE {
            return Err(HostError::InvalidInput(
                "`main` is the team lead; wait on your subagents, not on the lead".to_string(),
            ));
        }
        if !target.handle.starts_with(&prefix) {
            return Err(HostError::InvalidInput(format!(
                "`{}` is not one of your subagents; you can only wait on agents you spawned (or their subagents)",
                target.handle
            )));
        }
        if !targets
            .iter()
            .any(|(_, session)| *session == target.session)
        {
            targets.push((target.handle, target.session));
        }
    }
    Ok(targets)
}

fn is_terminal_roster_status(status: RosterStatus) -> bool {
    matches!(status, RosterStatus::Done | RosterStatus::Failed)
}

async fn handoff_generation(
    runtime: &dyn HostRuntime,
    session: SessionId,
) -> Result<u32, HostError> {
    let projection = runtime.session_store().read_projection(session).await?;
    Ok(handoff_projection(&projection).map_or(0, |h| h.generation))
}

async fn terminal_member_row(
    runtime: &dyn HostRuntime,
    child: SessionId,
) -> Result<Option<(MemberRunStatus, String)>, HostError> {
    let Some(parent) = runtime
        .session_store()
        .read_projection(child)
        .await?
        .session
        .parent
    else {
        return Ok(None);
    };
    let parent_proj = runtime.session_store().read_projection(parent).await?;
    Ok(members_projection(&parent_proj)
        .into_iter()
        .rev()
        .find(|row| row.child == Some(child))
        .map(|row| (row.status, row.summary)))
}

async fn last_assistant_message(
    runtime: &dyn HostRuntime,
    session: SessionId,
) -> Result<Option<MessageId>, HostError> {
    let projection = runtime.session_store().read_projection(session).await?;
    Ok(projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(|message| message.id))
}

fn is_relevant_wait_event(event: &Event, root: SessionId, caller: SessionId) -> bool {
    let lifecycle = matches!(
        ExtensionEvent::from_event(event),
        Some(
            ExtensionEvent::MailSent { .. }
                | ExtensionEvent::MailConsumed { .. }
                | ExtensionEvent::AgentActivityChanged { .. }
                | ExtensionEvent::AgentArchived { .. }
                | ExtensionEvent::AgentRestarted { .. }
                | ExtensionEvent::AgentRegistered { .. }
                | ExtensionEvent::ResidentWorkStarted { .. }
                | ExtensionEvent::ResidentWorkFinished { .. }
                | ExtensionEvent::SubagentReported { .. }
                | ExtensionEvent::MemberFinished { .. }
        )
    );
    lifecycle
        && event
            .session()
            .is_some_and(|session| session == root || session == caller)
}

fn extension_payload(event: &ExtensionEvent) -> serde_json::Value {
    match event.clone().into_event() {
        Event::Extension { payload, .. } => payload,
        _ => serde_json::Value::Null,
    }
}

fn parse_subagent_limits_from_config(config: &serde_json::Value) -> SubagentLimits {
    let mut limits = SubagentLimits::default();
    let section = config.get("subagents").unwrap_or(config);
    if let Some(max) = section
        .get("max_concurrency")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
    {
        limits.max_concurrency = max;
    }
    if let Some(budget) = section
        .get("per_run_budget")
        .and_then(serde_json::Value::as_u64)
    {
        limits.per_run_budget = budget;
    }
    if let Some(budget) = section
        .get("per_team_turn_budget")
        .and_then(serde_json::Value::as_u64)
    {
        limits.per_team_turn_budget = budget;
    }
    if let Some(budget) = section
        .get("per_team_message_budget")
        .and_then(serde_json::Value::as_u64)
    {
        limits.per_team_message_budget = budget;
    }
    limits
}
