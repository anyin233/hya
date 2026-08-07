//! Resident subagent lifecycle: long-lived, event-driven actors (ADR-0002).
//!
//! A [`ResidentSupervisor`] turns a subset of a team's sessions into **actors**
//! that are idle at *zero token cost* and woken only by inbound mail. It rides the
//! same [`EventBus`](crate::bus::EventBus) `MailSent` stream the mailbox already
//! publishes (Phase 3) — that is the wake seam — so nothing new polls.
//!
//! ## Guarantees (see the per-method docs for where each is enforced)
//! - **Zero idle cost.** Every resident parks on a `Notify`; the bus listener
//!   parks on `recv()`. No timers, no polling, no turns without a triggering mail.
//! - **Exactly one turn per wake, with coalescing.** A resident runs at most one
//!   turn at a time. Mail that arrives while it is mid-turn is *not lost*: it sets
//!   the slot's `pending` flag, and the resident runs exactly one follow-up turn
//!   after the current one, injecting every message accumulated since its cursor.
//! - **No self-wake.** Delivery excludes the sender's own handle, so an agent that
//!   posts to a channel it subscribes to does not wake itself.
//! - **Main-as-actor + quiescence.** The team root is registered as an actor and
//!   woken by child mail. When every resident goes idle and no mail is queued, the
//!   team is *quiescent* and the main agent is woken once to synthesize — unless it
//!   already synthesized with nothing new since (which is how termination is
//!   reached without an infinite re-wake loop).
//! - **Runaway kill.** Per-team turn and message budgets (on the
//!   [`SubagentGovernor`]) cancel the whole team when tripped.
//!
//! ## Concurrency model
//! All accounting (busy count, per-slot status/pending, quiescence, kill) lives
//! behind ONE `std::sync::Mutex<TeamState>` per team. Critical sections are short
//! and never `.await`; the long work (reading the projection, injecting mail,
//! running a turn) happens *outside* the lock. Because the decision to go idle and
//! the busy→0 quiescence check happen in the same locked section that observes
//! "no pending work", quiescence can never fire while a turn is running or mail is
//! queued, and it can never hang (the last resident to idle always runs the check).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hya_proto::{
    ActorClaim, Event, MailEndpoint, MemberId, OwnerRunId, RosterStatus, SessionId, SubagentMode,
};
use hya_tool::{AgentDef, ResolvedTool};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;
use crate::hooks::{HookDispatcher, scope_activation_hooks};
use crate::orchestrator::TeamBudget;
use crate::sidecar::{BoundSidecarFactory, SidecarHandle, SidecarStart};
use crate::{AgentResourcePolicy, TurnBinding};

/// Runtime pieces needed to resume a resident after process restart.
pub type ResolvedResidentRuntime = (
    TurnBinding,
    Arc<[AgentDef]>,
    AgentResourcePolicy,
    Option<Arc<dyn BoundSidecarFactory>>,
);

/// Durable recovery disposition for a resident actor after restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentRecovery {
    /// No pending work; park idle.
    Idle,
    /// Mail queued; run a turn from this inbox cursor.
    Queued {
        /// Projection cursor for unread mail.
        inbox_cursor: u64,
    },
    /// Was mid-turn; abort and optionally re-queue.
    AbortedRunning {
        /// Cursor at abort.
        inbox_cursor: u64,
        /// Whether more mail arrived after the aborted turn.
        queued_after: bool,
    },
}

/// Outcome of recovering one resident actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResidentRecoveryReport {
    /// Work disposition for the supervisor.
    pub work: ResidentRecovery,
    /// How many in-flight operations were aborted.
    pub aborted_operations: usize,
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
        let (work, aborted_operations) = self
            .recover_resident_actor_durable(recovered, root, handle)
            .await?;
        Ok(ResidentRecoveryReport {
            work: map_recovered_resident_work(work),
            aborted_operations,
        })
    }

    /// Classify durable resident work after its claim has been fenced by takeover.
    /// Running work is terminalized without retry; queued mail remains schedulable.
    pub async fn recover_resident_work(
        &self,
        recovered: &hya_store::RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<ResidentRecovery, CoreError> {
        let (work, _) = self
            .recover_resident_actor_durable(recovered, root, handle)
            .await?;
        Ok(map_recovered_resident_work(work))
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

/// Injected when the team quiesces so the main agent synthesizes autonomously.
const SYNTHESIS_DIRECTIVE: &str = "TEAM QUIESCED — every team member is idle and no mail is in flight. \
Review the team's results (roster, channels, and your inbox) and produce the final synthesized answer. \
If more work is genuinely required, delegate it; otherwise conclude.";

/// Per-slot activity, tracked in-memory (the durable mirror is `RosterStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotStatus {
    Idle,
    Busy,
}

/// One resident (or the main actor) inside a team.
struct SlotState {
    handle: String,
    agent: AgentSpec,
    /// Immutable runtime snapshot retained for every activation of this slot.
    binding: TurnBinding,
    agents: Option<Arc<[AgentDef]>>,
    resources: Option<AgentResourcePolicy>,
    /// In-process only: immutable guidance from the triggering spawn turn.
    /// Never durable; recovery paths leave this `None`.
    guidance: Option<Arc<str>>,
    /// Opaque request-scoped sidecar factory, retained only in memory.
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    is_main: bool,
    status: SlotStatus,
    /// New mail has arrived and a turn is owed.
    pending: bool,
    /// One-shot initial directive, retained only until the first run.
    initial_directive: Option<String>,
    /// (Main only) a synthesis directive is owed on the next turn.
    synth_pending: bool,
    /// Present for resident subagents; the main/root actor remains transient.
    claim: Option<ActorClaim>,
    /// Durable kill finalization completed for this slot's current claim.
    kill_finalized: bool,
    /// Slot-local cancellation, inherited from the team's cancellation token.
    cancel: CancellationToken,
    /// One-shot explicit stop request consumed by the resident task.
    stop_request: Option<StopRequest>,
    /// How many of this handle's inbox messages have already been injected.
    cursor: usize,
    notify: Arc<Notify>,
}

struct StopRequest {
    terminate: bool,
    reply: oneshot::Sender<Result<(), CoreError>>,
}

struct StopCompletion {
    outcome: Mutex<Option<Result<(), CoreError>>>,
    notify: Notify,
}

impl StopCompletion {
    fn new() -> Self {
        Self {
            outcome: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    fn outcome(&self) -> Option<Result<(), CoreError>> {
        match self.outcome.lock() {
            Ok(outcome) => outcome.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn complete(&self, result: Result<(), CoreError>) {
        let should_notify = match self.outcome.lock() {
            Ok(mut current) => {
                if current.is_none() {
                    *current = Some(result);
                    true
                } else {
                    false
                }
            }
            Err(poisoned) => {
                let mut current = poisoned.into_inner();
                if current.is_none() {
                    *current = Some(result);
                    true
                } else {
                    false
                }
            }
        };
        if should_notify {
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) -> Result<(), CoreError> {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = self.outcome() {
                return outcome;
            }
            notified.await;
        }
    }
}

impl SlotState {
    /// Whether this slot owes a turn (mail, a synthesis directive, or its initial
    /// directive). The single source of truth for "is there work?" under the lock.
    fn has_work(&self) -> bool {
        self.pending || self.synth_pending
    }
}

/// The mutable, lock-guarded state of one team.
struct TeamState {
    residents: HashMap<SessionId, SlotState>,
    stop_completions: HashMap<String, Arc<StopCompletion>>,
    /// Number of non-idle residents (incl. main). Quiescence = this is 0.
    busy: usize,
    killed: bool,
    /// Monotonic counter bumped on every unit of new work (mail, registration).
    /// Quiescence re-fires only when this advances past the last synthesis.
    work_seq: u64,
    /// `work_seq` captured at the last synthesis wake. Initialized to `u64::MAX`
    /// so the FIRST quiescence always fires (any real `work_seq` differs from it).
    last_synth_work_seq: u64,
    main_session: Option<SessionId>,
    kill_reason: Option<String>,
    /// Guards the one-time release of the team's governor counters.
    team_budget_released: bool,
}

/// Everything one turn needs, snapshotted atomically under the team lock so the
/// (long, unlocked) turn runs against a stable plan.
struct RunPlan {
    agent: AgentSpec,
    binding: TurnBinding,
    agents: Option<Arc<[AgentDef]>>,
    resources: Option<AgentResourcePolicy>,
    guidance: Option<Arc<str>>,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    handle: String,
    is_main: bool,
    /// Inbox messages before this index are already injected.
    cursor: usize,
    /// (Main only) inject the synthesis directive before the turn.
    synth: bool,
    initial_directive: Option<String>,
    claim: Option<ActorClaim>,
    cancel: CancellationToken,
}

enum ResidentRuntimeContext {
    Bound(TurnBinding),
    Resolved {
        binding: TurnBinding,
        agents: Arc<[AgentDef]>,
        resources: AgentResourcePolicy,
    },
}

struct ResidentActivation {
    initial: Option<String>,
    guidance: Option<Arc<str>>,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
}

/// What a resident task should do next, decided atomically under the team lock.
enum Action {
    /// Run exactly one turn per the snapshotted [`RunPlan`].
    Run(Box<RunPlan>),
    /// No work owed; the slot just transitioned to idle (`became_idle` gates the
    /// roster activity emission so it fires once per idle transition).
    Idle {
        handle: String,
        claim: Option<ActorClaim>,
        became_idle: bool,
    },
    /// Explicitly stop one resident and report process cleanup to the caller.
    StopResident {
        terminate: bool,
        reply: oneshot::Sender<Result<(), CoreError>>,
    },
    /// The team was killed; the resident task must exit. `killed_now` is set for
    /// the single caller that observed the transition, so kill side-effects run once.
    Stop { killed_now: bool },
    /// The team was killed, but this slot's durable finalization is still pending.
    WaitForKillFinalization,
}

/// A single team's actor group: its residents, budgets, and cancellation.
struct TeamActor {
    root: SessionId,
    engine: Arc<SessionEngine>,
    cancel: CancellationToken,
    state: Mutex<TeamState>,
}

impl TeamActor {
    fn lock(&self) -> std::sync::MutexGuard<'_, TeamState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    async fn record_activity(
        &self,
        claim: Option<&ActorClaim>,
        handle: String,
        status: RosterStatus,
        current_task: Option<String>,
    ) -> Result<(), CoreError> {
        match claim {
            Some(claim) => {
                self.engine
                    .commit_resident_mutation(
                        claim,
                        self.root,
                        vec![Event::AgentActivityChanged {
                            session: self.root,
                            handle,
                            status,
                            current_task,
                        }],
                    )
                    .await
            }
            None => {
                self.engine
                    .record_agent_activity(self.root, handle, status, current_task)
                    .await
            }
        }
    }

    /// Decide (atomically) what the resident on `session` does next. Charges the
    /// per-team turn budget on the way into a `Run`; a trip kills the team.
    fn next_action(&self, session: SessionId) -> Action {
        let mut st = self.lock();
        let killed = st.killed;
        let Some(slot) = st.residents.get_mut(&session) else {
            return Action::Stop { killed_now: false };
        };
        if let Some(request) = slot.stop_request.take() {
            return Action::StopResident {
                terminate: request.terminate,
                reply: request.reply,
            };
        }
        if killed {
            if !slot.kill_finalized {
                return Action::WaitForKillFinalization;
            }
            return Action::Stop { killed_now: false };
        }
        if slot.has_work() {
            let synth = slot.synth_pending;
            let claim = slot.claim;
            let cursor = slot.cursor;
            let agent = slot.agent.clone();
            let binding = slot.binding.clone();
            let agents = slot.agents.clone();
            let resources = slot.resources.clone();
            let guidance = slot.guidance.clone();
            let sidecar_factory = slot.sidecar_factory.clone();
            let handle = slot.handle.clone();
            let is_main = slot.is_main;
            let initial_directive = slot.initial_directive.take();
            let cancel = slot.cancel.child_token();
            slot.pending = false;
            slot.synth_pending = false;
            if slot.status == SlotStatus::Idle {
                slot.status = SlotStatus::Busy;
                st.busy += 1;
            }
            // Charge the per-team turn budget; a runaway (endless re-wake) trips it.
            if let Some(gov) = self.engine.governor()
                && gov.charge_team_turn(self.root) == TeamBudget::Exceeded
            {
                self.kill_locked(&mut st, "per-team turn budget exceeded");
                return Action::Stop { killed_now: true };
            }
            Action::Run(Box::new(RunPlan {
                agent,
                binding,
                agents,
                resources,
                guidance,
                sidecar_factory,
                handle,
                is_main,
                cursor,
                synth,
                initial_directive,
                claim,
                cancel,
            }))
        } else {
            let handle = slot.handle.clone();
            let claim = slot.claim;
            let became_idle = slot.status == SlotStatus::Busy;
            if became_idle {
                slot.status = SlotStatus::Idle;
                st.busy = st.busy.saturating_sub(1);
                self.maybe_fire_quiescence(&mut st);
            }
            Action::Idle {
                handle,
                claim,
                became_idle,
            }
        }
    }

    fn finish_run(&self, session: SessionId) {
        let mut state = self.lock();
        let has_work = match state.residents.get_mut(&session) {
            Some(slot) => {
                slot.status = SlotStatus::Idle;
                slot.has_work()
            }
            None => return,
        };
        state.busy = state.busy.saturating_sub(1);
        if !has_work {
            self.maybe_fire_quiescence(&mut state);
        }
    }

    fn replace_recovered_claim(
        &self,
        session: SessionId,
        expected: ActorClaim,
        recovered: ActorClaim,
        disposition: ResidentRecovery,
    ) -> Result<(), CoreError> {
        let (cursor, pending) = match disposition {
            ResidentRecovery::Idle => (None, false),
            ResidentRecovery::Queued { inbox_cursor } => (
                Some(usize::try_from(inbox_cursor).map_err(|_| {
                    CoreError::Invalid("resident inbox cursor exceeds usize".to_string())
                })?),
                true,
            ),
            ResidentRecovery::AbortedRunning {
                inbox_cursor,
                queued_after,
            } => (
                Some(usize::try_from(inbox_cursor).map_err(|_| {
                    CoreError::Invalid("resident inbox cursor exceeds usize".to_string())
                })?),
                queued_after,
            ),
        };
        let mut state = self.lock();
        let slot = state
            .residents
            .get_mut(&session)
            .ok_or_else(|| CoreError::Invalid("resident recovery slot missing".to_string()))?;
        if slot.claim != Some(expected) {
            return Err(CoreError::Invalid(
                "resident recovery claim changed before replacement".to_string(),
            ));
        }
        slot.claim = Some(recovered);
        slot.kill_finalized = false;
        if let Some(cursor) = cursor {
            slot.cursor = cursor;
        }
        slot.pending |= pending;
        Ok(())
    }

    fn remove_slot(&self, session: SessionId) -> Result<(), CoreError> {
        {
            let mut state = self.lock();
            let slot = state
                .residents
                .remove(&session)
                .ok_or_else(|| CoreError::Invalid("resident stop slot missing".to_string()))?;
            if slot.status == SlotStatus::Busy {
                state.busy = state.busy.saturating_sub(1);
            }
            if state.main_session == Some(session) {
                state.main_session = None;
            }
        }
        self.release_team_budget_if_empty();
        Ok(())
    }

    fn release_team_budget_if_empty(&self) {
        let should_release = {
            let mut state = self.lock();
            if state.killed && state.residents.is_empty() && !state.team_budget_released {
                state.team_budget_released = true;
                true
            } else {
                false
            }
        };
        if should_release && let Some(governor) = self.engine.governor() {
            governor.release_team(self.root);
        }
    }

    fn mark_kill_finalized(&self, session: SessionId, handle: &str, claim: Option<&ActorClaim>) {
        let mut state = self.lock();
        let Some(slot) = state.residents.get_mut(&session) else {
            return;
        };
        let same_claim = match (slot.claim.as_ref(), claim) {
            (Some(current), Some(expected)) => current == expected,
            (None, None) => true,
            _ => false,
        };
        if slot.handle == handle && same_claim {
            slot.kill_finalized = true;
            slot.notify.notify_one();
        }
    }

    fn kill_finalized(&self, session: SessionId) -> bool {
        let state = self.lock();
        state
            .residents
            .get(&session)
            .is_none_or(|slot| slot.kill_finalized)
    }

    fn remove_stop_completion(&self, handle: &str, completion: &Arc<StopCompletion>) {
        let mut state = self.lock();
        if state
            .stop_completions
            .get(handle)
            .is_some_and(|current| Arc::ptr_eq(current, completion))
        {
            state.stop_completions.remove(handle);
        }
    }

    /// If the team just went fully idle with new work since the last synthesis,
    /// wake the main actor to synthesize. Pure in-memory + `Notify`, safe under
    /// the lock. Not firing when `work_seq` is unchanged is what guarantees
    /// termination: main's own synthesis (which produces no new work) leaves
    /// `work_seq` equal, so the next idle transition is a no-op (the team is done).
    fn maybe_fire_quiescence(&self, st: &mut TeamState) {
        if st.killed || st.busy != 0 {
            return;
        }
        if st.work_seq == st.last_synth_work_seq {
            return; // nothing new since last synthesis → team is done, parked idle
        }
        st.last_synth_work_seq = st.work_seq;
        if let Some(main_session) = st.main_session
            && let Some(main_slot) = st.residents.get_mut(&main_session)
        {
            main_slot.synth_pending = true;
            main_slot.pending = true;
            main_slot.notify.notify_one();
        }
    }

    /// Mark the team killed and cancel every in-flight turn. Idempotent. Wakes all
    /// residents so their parked tasks observe `killed` and exit.
    fn kill_locked(&self, st: &mut TeamState, reason: &str) {
        if st.killed {
            return;
        }
        st.killed = true;
        st.kill_reason = Some(reason.to_string());
        self.cancel.cancel();
        for slot in st.residents.values() {
            slot.notify.notify_one();
        }
    }

    /// Kill the team from an async context (message-budget trip). Records the
    /// terminal `Failed` roster status for every member so observers see the reason.
    async fn kill(&self, reason: &str) {
        let (already, residents) = {
            let mut st = self.lock();
            let already = st.killed;
            self.kill_locked(&mut st, reason);
            let residents = st
                .residents
                .iter()
                .map(|(session, slot)| (*session, slot.handle.clone(), slot.claim))
                .collect::<Vec<_>>();
            (already, residents)
        };
        if already {
            return;
        }
        self.emit_kill(&residents, reason).await;
    }

    /// Atomically terminalize each claimed resident, retaining failed slots for a
    /// later explicit stop retry. Claim-less main actors keep the existing activity
    /// event behavior and may clean up locally after the event attempt.
    async fn emit_kill(&self, residents: &[(SessionId, String, Option<ActorClaim>)], reason: &str) {
        for (session, handle, claim) in residents {
            let finalized = match claim {
                Some(claim) => self
                    .engine
                    .finalize_resident_failure(claim, self.root, handle, reason)
                    .await
                    .is_ok(),
                None => {
                    let _ = self
                        .record_activity(
                            None,
                            handle.clone(),
                            RosterStatus::Failed,
                            Some(reason.to_string()),
                        )
                        .await;
                    true
                }
            };
            if finalized {
                self.mark_kill_finalized(*session, handle, claim.as_ref());
            }
        }
    }

    /// Run exactly one turn for `session`: inject the initial directive (once), the
    /// synthesis directive (main, on quiescence), and every inbox message since the
    /// cursor, then advance the cursor and stream one turn. All of this is coalesced
    /// into a single turn — many queued messages produce one turn, never several.
    async fn run_one_turn(
        &self,
        session: SessionId,
        plan: RunPlan,
        sidecar_tools: Arc<[ResolvedTool]>,
    ) -> Result<(), CoreError> {
        let RunPlan {
            agent,
            binding,
            agents,
            resources,
            guidance,
            sidecar_factory: _,
            handle,
            is_main,
            cursor,
            synth,
            initial_directive,
            claim,
            cancel,
        } = plan;
        // Snapshot new inbox mail for this handle (folded before its wake, so it is
        // already visible here).
        let projection = self.engine.read_projection(self.root).await?;
        let inbox_len = projection
            .team
            .inboxes
            .get(&handle)
            .map_or(0, |inbox| inbox.len());
        let new_mail: Vec<(String, String)> = projection
            .team
            .inboxes
            .get(&handle)
            .map(|inbox| {
                inbox
                    .iter()
                    .skip(cursor)
                    .map(|m| (m.from.clone(), m.body.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let task_label = if synth && is_main {
            "synthesizing".to_string()
        } else if let Some((from, _)) = new_mail.first() {
            format!("mail from {from}")
        } else {
            "working".to_string()
        };
        if let Some(claim) = claim.as_ref() {
            self.engine
                .commit_resident_mutation(
                    claim,
                    self.root,
                    vec![
                        Event::ResidentWorkStarted {
                            session: self.root,
                            actor_session: session,
                            handle: handle.clone(),
                            epoch: claim.epoch,
                            inbox_through: u64::try_from(inbox_len).unwrap_or(u64::MAX),
                        },
                        Event::AgentActivityChanged {
                            session: self.root,
                            handle: handle.clone(),
                            status: RosterStatus::Busy,
                            current_task: Some(task_label),
                        },
                    ],
                )
                .await?;
        } else {
            self.record_activity(None, handle.clone(), RosterStatus::Busy, Some(task_label))
                .await?;
        }
        if synth && is_main {
            match claim.as_ref() {
                Some(claim) => {
                    self.engine
                        .inject_system_message_for_actor(
                            claim,
                            session,
                            SYNTHESIS_DIRECTIVE.to_string(),
                        )
                        .await?;
                }
                None => {
                    self.engine
                        .inject_system_message(session, SYNTHESIS_DIRECTIVE.to_string())
                        .await?;
                }
            }
        }
        if let Some(initial) = initial_directive {
            match claim.as_ref() {
                Some(claim) => {
                    self.engine
                        .admit_user_prompt_for_actor(claim, session, initial)
                        .await?;
                }
                None => {
                    self.engine.admit_user_prompt(session, initial).await?;
                }
            }
        }
        for (from, body) in &new_mail {
            let prompt = format!("[mail from {from}] {body}");
            match claim.as_ref() {
                Some(claim) => {
                    self.engine
                        .admit_user_prompt_for_actor(claim, session, prompt)
                        .await?;
                }
                None => {
                    self.engine.admit_user_prompt(session, prompt).await?;
                }
            }
        }
        // Advance the cursor so a follow-up turn never re-injects the same mail.
        {
            let mut st = self.lock();
            if let Some(slot) = st.residents.get_mut(&session) {
                slot.cursor = slot.cursor.max(inbox_len);
            }
        }
        // Exactly one turn, under the team-wide cancel so a budget kill stops it.
        match (claim.as_ref(), agents, resources) {
            (Some(claim), Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools_for_actor(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        claim,
                        cancel.clone(),
                        guidance,
                    )
                    .await?;
            }
            (None, Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        cancel.clone(),
                        guidance,
                    )
                    .await?;
            }
            (Some(claim), _, _) => {
                self.engine
                    .run_bound_turn_for_actor(
                        session,
                        &agent,
                        binding,
                        claim,
                        cancel.clone(),
                        guidance,
                    )
                    .await?;
            }
            (None, _, _) => {
                self.engine
                    .run_bound_turn(session, &agent, binding, cancel, guidance)
                    .await?;
            }
        }
        Ok(())
    }

    /// Resolve a `MailSent`'s recipient sessions, EXCLUDING the sender's own handle
    /// (self-wake avoidance) and any handle that is not a registered resident.
    async fn recipients(&self, from: &str, to: &MailEndpoint) -> Vec<SessionId> {
        let Ok(projection) = self.engine.read_projection(self.root).await else {
            return Vec::new();
        };
        let handles: Vec<String> = match to {
            MailEndpoint::Handle(handle) => {
                if handle == from {
                    Vec::new() // a self-addressed direct mail never self-wakes
                } else {
                    vec![handle.clone()]
                }
            }
            MailEndpoint::Channel(channel) => projection
                .team
                .channels
                .get(channel)
                .map(|ch| {
                    ch.members
                        .iter()
                        .filter(|m| m.as_str() != from)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        };
        let st = self.lock();
        handles
            .into_iter()
            .filter_map(|handle| {
                projection
                    .team
                    .roster
                    .get(&handle)
                    .map(|entry| entry.session)
                    .filter(|session| st.residents.contains_key(session))
            })
            .collect()
    }

    /// Handle one `MailSent` for this team: charge the message budget, count the
    /// work, and deliver a wake to every resident recipient (never the sender).
    async fn on_mail(&self, from: &str, to: &MailEndpoint) {
        if let Some(gov) = self.engine.governor()
            && gov.charge_team_message(self.root) == TeamBudget::Exceeded
        {
            self.kill("per-team message budget exceeded").await;
            return;
        }
        {
            let mut st = self.lock();
            if st.killed {
                return;
            }
            st.work_seq = st.work_seq.saturating_add(1);
        }
        let recipients = self.recipients(from, to).await;
        let mut st = self.lock();
        for session in recipients {
            if let Some(slot) = st.residents.get_mut(&session) {
                slot.pending = true;
                slot.notify.notify_one();
            }
        }
    }

    /// Recover from a bus lag: a dropped `MailSent` never ran [`on_mail`], so the
    /// affected slot's `pending` was never set and a bare notify would just idle
    /// again. Instead, compare each resident's inbox length against its cursor and
    /// re-arm (set `pending` + notify) any slot with genuinely unconsumed mail — so
    /// no wake is lost, and quiescent slots with nothing new are left parked.
    async fn recover(&self) {
        let Ok(projection) = self.engine.read_projection(self.root).await else {
            return;
        };
        let mut st = self.lock();
        if st.killed {
            return;
        }
        for slot in st.residents.values_mut() {
            let inbox_len = projection
                .team
                .inboxes
                .get(&slot.handle)
                .map_or(0, |inbox| inbox.len());
            if inbox_len > slot.cursor {
                slot.pending = true;
                slot.notify.notify_one();
            }
        }
    }
}

/// The resident actor loop for one session: park at zero cost, then run turns
/// (with follow-ups for mail that arrived mid-turn) until it owes none, then park
/// again. Exits when the team is killed or the supervisor is dropped.
async fn resident_task(team: Arc<TeamActor>, session: SessionId, notify: Arc<Notify>) {
    let mut sidecar_handle: Option<Box<dyn SidecarHandle>> = None;
    let mut sidecar_tools: Arc<[ResolvedTool]> = Arc::from([]);
    let mut sidecar_hooks: Option<Arc<dyn HookDispatcher>> = None;
    let mut stop_requires_terminate = false;
    let mut stop_cleanup_pending = false;
    let mut stop_cleanup_requires_request = false;
    loop {
        if stop_cleanup_pending {
            notify.notified().await;
            if stop_cleanup_requires_request {
                let waiting_for_request = {
                    let state = team.lock();
                    state
                        .residents
                        .get(&session)
                        .is_some_and(|slot| slot.stop_request.is_none())
                };
                if waiting_for_request {
                    continue;
                }
            }
        } else if let Some(loss_token) = sidecar_handle
            .as_ref()
            .and_then(|handle| handle.loss_token())
        {
            tokio::select! {
                biased;
                _ = loss_token.cancelled() => {
                    sidecar_tools = Arc::from([]);
                    sidecar_hooks = None;
                    if let Some(mut handle) = sidecar_handle.take() {
                        let _ = handle.terminate().await;
                    }
                    continue;
                }
                _ = notify.notified() => {}
            }
        } else {
            notify.notified().await;
        }
        loop {
            match team.next_action(session) {
                Action::Run(plan) => {
                    let handle = plan.handle.clone();
                    let claim = plan.claim;
                    if sidecar_handle
                        .as_ref()
                        .is_some_and(|cached| !cached.is_healthy())
                        && let Some(mut stale_handle) = sidecar_handle.take()
                    {
                        sidecar_tools = Arc::from([]);
                        sidecar_hooks = None;
                        if let Err(err) = stale_handle.terminate().await {
                            let _ = team
                                .record_activity(
                                    claim.as_ref(),
                                    handle,
                                    RosterStatus::Failed,
                                    Some(format!("turn error: {err}")),
                                )
                                .await;
                            team.finish_run(session);
                            continue;
                        }
                    }
                    if sidecar_handle.is_none()
                        && let Some(factory) = plan.sidecar_factory.clone()
                    {
                        let activation = async {
                            let mut handle = factory.start(SidecarStart::resident()).await?;
                            if let Err(error) = handle.ready().await {
                                let _ = handle.terminate().await;
                                return Err(error);
                            }
                            let tool_bindings = handle.tool_bindings();
                            let hook_dispatcher = handle.hook_dispatcher();
                            Ok::<
                                (
                                    Box<dyn SidecarHandle>,
                                    Arc<[ResolvedTool]>,
                                    Option<Arc<dyn HookDispatcher>>,
                                ),
                                CoreError,
                            >((handle, tool_bindings, hook_dispatcher))
                        }
                        .await;
                        match activation {
                            Ok((handle, tool_bindings, hook_dispatcher)) => {
                                sidecar_tools = tool_bindings;
                                sidecar_hooks = hook_dispatcher;
                                sidecar_handle = Some(handle);
                            }
                            Err(err) => {
                                let reason = format!("turn error: {err}");
                                if let Some(claim) = claim.as_ref() {
                                    match team
                                        .engine
                                        .finalize_resident_failure(
                                            claim, team.root, &handle, &reason,
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            let _ = team.remove_slot(session);
                                            return;
                                        }
                                        Err(_error) => {
                                            stop_cleanup_pending = true;
                                            team.finish_run(session);
                                            break;
                                        }
                                    }
                                }
                                let _ = team
                                    .record_activity(
                                        None,
                                        handle,
                                        RosterStatus::Failed,
                                        Some(reason),
                                    )
                                    .await;
                                let _ = team.remove_slot(session);
                                return;
                            }
                        }
                    }
                    let loss_token = sidecar_handle
                        .as_ref()
                        .and_then(|handle| handle.loss_token());
                    let (turn_result, sidecar_lost) = match (sidecar_hooks.clone(), loss_token) {
                        (Some(hooks), Some(loss_token)) => {
                            tokio::select! {
                                biased;
                                _ = loss_token.cancelled() => (Err(CoreError::Cancelled), true),
                                result = scope_activation_hooks(
                                    session,
                                    hooks,
                                    team.run_one_turn(session, *plan, Arc::clone(&sidecar_tools)),
                                ) => (result, false),
                            }
                        }
                        (Some(hooks), None) => (
                            scope_activation_hooks(
                                session,
                                hooks,
                                team.run_one_turn(session, *plan, Arc::clone(&sidecar_tools)),
                            )
                            .await,
                            false,
                        ),
                        (None, Some(loss_token)) => {
                            tokio::select! {
                                biased;
                                _ = loss_token.cancelled() => (Err(CoreError::Cancelled), true),
                                result = team.run_one_turn(session, *plan, Arc::clone(&sidecar_tools)) => (result, false),
                            }
                        }
                        (None, None) => (
                            team.run_one_turn(session, *plan, Arc::clone(&sidecar_tools))
                                .await,
                            false,
                        ),
                    };
                    let running_loss = claim.is_some()
                        && matches!(&turn_result, Err(CoreError::Cancelled))
                        && (sidecar_lost
                            || sidecar_handle
                                .as_ref()
                                .is_some_and(|cached| !cached.is_healthy()));
                    if running_loss {
                        let Some(old_claim) = claim else {
                            team.finish_run(session);
                            continue;
                        };
                        let recovered = match team
                            .engine
                            .store()
                            .recover_claim(old_claim.actor_id, old_claim.owner_run_id)
                            .await
                        {
                            Ok(recovered) => recovered,
                            Err(error) => {
                                sidecar_tools = Arc::from([]);
                                sidecar_hooks = None;
                                if let Some(mut stale_handle) = sidecar_handle.take() {
                                    let _ = stale_handle.terminate().await;
                                }
                                let reason = format!("resident recovery failed: {error}");
                                match team
                                    .engine
                                    .finalize_resident_failure(
                                        &old_claim, team.root, &handle, &reason,
                                    )
                                    .await
                                {
                                    Ok(()) => match team.remove_slot(session) {
                                        Ok(()) | Err(_) => return,
                                    },
                                    Err(_error) => {
                                        stop_cleanup_pending = true;
                                        team.finish_run(session);
                                        break;
                                    }
                                }
                            }
                        };
                        sidecar_tools = Arc::from([]);
                        sidecar_hooks = None;
                        let termination_error = match sidecar_handle.take() {
                            Some(mut stale_handle) => {
                                stale_handle.terminate().await.err().map(|error| {
                                    CoreError::Invalid(format!(
                                        "terminate resident sidecar during recovery: {error}"
                                    ))
                                })
                            }
                            None => Some(CoreError::Invalid(
                                "resident sidecar disappeared during recovery".to_string(),
                            )),
                        };
                        let report = match team
                            .engine
                            .recover_resident_actor(&recovered, team.root, &handle)
                            .await
                        {
                            Ok(report) => report,
                            Err(error) => {
                                let claim_replaced = team
                                    .replace_recovered_claim(
                                        session,
                                        old_claim,
                                        recovered.claim,
                                        ResidentRecovery::Idle,
                                    )
                                    .is_ok();
                                let reason = format!("resident recovery failed: {error}");
                                match team
                                    .engine
                                    .finalize_resident_failure(
                                        &recovered.claim,
                                        team.root,
                                        &handle,
                                        &reason,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        if claim_replaced {
                                            let _ = team.remove_slot(session);
                                        }
                                        return;
                                    }
                                    Err(_error) => {
                                        stop_cleanup_pending = true;
                                        if claim_replaced {
                                            team.finish_run(session);
                                        }
                                        break;
                                    }
                                }
                            }
                        };
                        if let Err(_error) = team.replace_recovered_claim(
                            session,
                            old_claim,
                            recovered.claim,
                            report.work,
                        ) {
                            team.finish_run(session);
                            return;
                        }
                        if termination_error.is_some() {
                            match team
                                .engine
                                .finalize_resident_stop(&recovered.claim, team.root, &handle)
                                .await
                            {
                                Ok(()) => {
                                    // A missing slot means local cleanup already completed; the
                                    // durable finalizer has already made the stop terminal.
                                    match team.remove_slot(session) {
                                        Ok(()) | Err(_) => return,
                                    }
                                }
                                Err(_error) => {
                                    stop_cleanup_pending = true;
                                    team.finish_run(session);
                                    break;
                                }
                            }
                        }
                        team.finish_run(session);
                        continue;
                    }
                    match turn_result {
                        Ok(()) => {
                            let _ = team
                                .record_activity(claim.as_ref(), handle, RosterStatus::Idle, None)
                                .await;
                        }
                        Err(err) => {
                            // A turn error must not wedge the actor: record it and let
                            // the loop re-decide (it will idle if nothing else is owed).
                            let _ = team
                                .record_activity(
                                    claim.as_ref(),
                                    handle,
                                    RosterStatus::Failed,
                                    Some(format!("turn error: {err}")),
                                )
                                .await;
                        }
                    }
                    team.finish_run(session);
                }
                Action::StopResident { terminate, reply } => {
                    stop_requires_terminate |= terminate;
                    let cleanup = match sidecar_handle.as_mut() {
                        Some(handle) if stop_requires_terminate => handle.terminate().await,
                        Some(handle) => handle.shutdown().await,
                        None => Ok(()),
                    };
                    if let Err(error) = cleanup {
                        stop_cleanup_pending = true;
                        let _ = reply.send(Err(error));
                        break;
                    }
                    stop_cleanup_pending = false;
                    sidecar_handle.take();
                    sidecar_tools = Arc::from([]);
                    sidecar_hooks = None;
                    let result = team.remove_slot(session);
                    let should_return = result.is_ok();
                    let _ = reply.send(result);
                    if should_return {
                        return;
                    }
                    break;
                }
                Action::Idle {
                    handle,
                    claim,
                    became_idle,
                } => {
                    if became_idle {
                        let _ = team
                            .record_activity(claim.as_ref(), handle, RosterStatus::Idle, None)
                            .await;
                    }
                    break;
                }
                Action::Stop { killed_now } => {
                    if killed_now {
                        let (residents, reason) = {
                            let st = team.lock();
                            (
                                st.residents
                                    .iter()
                                    .map(|(session, slot)| {
                                        (*session, slot.handle.clone(), slot.claim)
                                    })
                                    .collect::<Vec<_>>(),
                                st.kill_reason.clone().unwrap_or_default(),
                            )
                        };
                        team.emit_kill(&residents, &reason).await;
                    }
                    if !team.kill_finalized(session) {
                        break;
                    }
                    if let Some(handle) = sidecar_handle.as_mut()
                        && handle.terminate().await.is_err()
                    {
                        stop_requires_terminate = true;
                        stop_cleanup_pending = true;
                        stop_cleanup_requires_request = true;
                        break;
                    }
                    sidecar_handle.take();
                    let _ = team.remove_slot(session);
                    return;
                }
                Action::WaitForKillFinalization => break,
            }
        }
    }
}

/// Drives resident (long-lived actor) subagents for every team that has one.
///
/// Constructed once per runtime via [`ResidentSupervisor::start`], which spawns a
/// single bus listener. Teams are created lazily as residents/main are registered;
/// transient-only teams are never tracked, so the default `run_team` path is
/// completely unaffected.
pub struct ResidentSupervisor {
    engine: Arc<SessionEngine>,
    owner_run_id: OwnerRunId,
    teams: Mutex<HashMap<SessionId, Arc<TeamActor>>>,
}

impl ResidentSupervisor {
    /// Build the supervisor and spawn its bus listener. The returned `Arc` is the
    /// registration handle used by the spawn path.
    ///
    /// The bus is subscribed *synchronously* here, before returning, so any mail
    /// published after `start` is guaranteed to be observed (no lost-wake race with
    /// the listener task's startup).
    #[must_use]
    pub fn start(engine: Arc<SessionEngine>) -> Arc<Self> {
        Self::start_with_owner(engine, OwnerRunId::new())
    }

    #[must_use]
    /// Start a supervisor owned by `owner_run_id` (cancels with the owner).
    pub fn start_with_owner(engine: Arc<SessionEngine>, owner_run_id: OwnerRunId) -> Arc<Self> {
        let rx = engine.bus().subscribe();
        let supervisor = Arc::new(Self {
            engine,
            owner_run_id,
            teams: Mutex::new(HashMap::new()),
        });
        let listener = supervisor.clone();
        tokio::spawn(async move { listener.run_bus(rx).await });
        supervisor
    }

    fn teams(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Arc<TeamActor>>> {
        match self.teams.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The single, zero-cost wake loop: park on the bus, route each `MailSent` to
    /// its team. On a broadcast lag, re-arm every team's residents so no wake is
    /// lost (the cursor makes re-arming safe — a spurious wake with no new mail
    /// simply idles again).
    async fn run_bus(
        self: Arc<Self>,
        mut rx: tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
    ) {
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    if let Event::MailSent {
                        session: root,
                        from,
                        to,
                        ..
                    } = &envelope.event
                    {
                        let team = self.teams().get(root).cloned();
                        if let Some(team) = team {
                            team.on_mail(from, to).await;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let teams: Vec<Arc<TeamActor>> = self.teams().values().cloned().collect();
                    for team in teams {
                        team.recover().await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// Get or create the team actor for `root`, sharing the team-wide cancellation.
    fn team_for(&self, root: SessionId) -> Arc<TeamActor> {
        self.teams()
            .entry(root)
            .or_insert_with(|| {
                Arc::new(TeamActor {
                    root,
                    engine: self.engine.clone(),
                    cancel: CancellationToken::new(),
                    state: Mutex::new(TeamState {
                        residents: HashMap::new(),
                        stop_completions: HashMap::new(),
                        busy: 0,
                        killed: false,
                        work_seq: 0,
                        last_synth_work_seq: u64::MAX,
                        main_session: None,
                        kill_reason: None,
                        team_budget_released: false,
                    }),
                })
            })
            .clone()
    }

    /// The team-wide cancellation token for `root`, if the team is tracked. Exposed
    /// so the runtime/TUI can observe or force a team shutdown.
    #[must_use]
    pub fn team_cancel(&self, root: SessionId) -> Option<CancellationToken> {
        self.teams().get(&root).and_then(|team| {
            let state = team.lock();
            (!state.residents.is_empty()).then(|| team.cancel.clone())
        })
    }

    /// Stop and deregister a resident handle under `root`.
    pub async fn stop_resident(&self, root: SessionId, handle: &str) -> Result<(), CoreError> {
        let team = self.teams().get(&root).cloned();
        let Some(team) = team else {
            return self.stop_terminal_projection(root, handle).await;
        };

        let mut selected = None;
        let mut waiting = None;
        let mut selection_error = None;
        {
            let mut state = team.lock();
            if let Some(completion) = state.stop_completions.get(handle) {
                waiting = Some(completion.clone());
            } else if let Some((&session, slot)) = state
                .residents
                .iter_mut()
                .find(|(_, slot)| slot.handle == handle)
            {
                if slot.is_main {
                    selection_error = Some(CoreError::Invalid(
                        "cannot stop resident main actor".to_string(),
                    ));
                } else if let Some(claim) = slot.claim {
                    let completion = Arc::new(StopCompletion::new());
                    let terminate = slot.status == SlotStatus::Busy;
                    let (reply, receiver) = oneshot::channel();
                    let stop_request = StopRequest { terminate, reply };
                    slot.pending = false;
                    slot.initial_directive = None;
                    slot.synth_pending = false;
                    slot.cancel.cancel();
                    slot.notify.notify_one();
                    state
                        .stop_completions
                        .insert(handle.to_string(), completion.clone());
                    selected = Some((session, claim, stop_request, receiver, completion));
                } else {
                    selection_error = Some(CoreError::Invalid(
                        "resident has no active claim".to_string(),
                    ));
                }
            }
        }
        if let Some(error) = selection_error {
            return Err(error);
        }
        if let Some(completion) = waiting {
            return completion.wait().await;
        }
        let Some((session, claim, stop_request, receiver, completion)) = selected else {
            return self.stop_terminal_projection(root, handle).await;
        };

        let leader_team = team.clone();
        let leader_handle = handle.to_string();
        let leader_completion = completion.clone();
        tokio::spawn(async move {
            let durable = leader_team
                .engine
                .finalize_resident_stop(&claim, root, &leader_handle)
                .await;
            let Err(durable_error) = durable else {
                let post_commit = {
                    let mut state = leader_team.lock();
                    match state.residents.get_mut(&session) {
                        Some(slot)
                            if slot.handle == leader_handle
                                && slot.claim == Some(claim)
                                && slot.stop_request.is_none() =>
                        {
                            slot.stop_request = Some(stop_request);
                            slot.notify.notify_one();
                            Ok(Some(receiver))
                        }
                        Some(_) => Err(CoreError::Invalid(
                            "resident stop slot claim changed before cleanup".to_string(),
                        )),
                        // The durable finalizer can race the activation-failure path's local
                        // cleanup.  A missing slot here means that cleanup already completed.
                        None => Ok(None),
                    }
                };
                let cleanup = match post_commit {
                    Err(error) => Err(error),
                    Ok(None) => Ok(()),
                    Ok(Some(receiver)) => match receiver.await {
                        Ok(result) => result,
                        Err(_) => {
                            let slot_absent = {
                                let state = leader_team.lock();
                                !state.residents.contains_key(&session)
                            };
                            if slot_absent {
                                Ok(())
                            } else {
                                Err(CoreError::Invalid(
                                    "resident stop task exited before cleanup".to_string(),
                                ))
                            }
                        }
                    },
                };
                leader_completion.complete(cleanup);
                leader_team.remove_stop_completion(&leader_handle, &leader_completion);
                return;
            };
            leader_completion.complete(Err(durable_error));
            leader_team.remove_stop_completion(&leader_handle, &leader_completion);
        });
        completion.wait().await
    }

    async fn stop_terminal_projection(
        &self,
        root: SessionId,
        handle: &str,
    ) -> Result<(), CoreError> {
        let projection = self.engine.read_projection(root).await?;
        let Some(entry) = projection.team.roster.get(handle) else {
            return Err(CoreError::Invalid(format!("unknown resident `{handle}`")));
        };
        if entry.mode == SubagentMode::Resident
            && matches!(entry.status, RosterStatus::Done | RosterStatus::Failed)
        {
            Ok(())
        } else if entry.mode != SubagentMode::Resident {
            Err(CoreError::Invalid(format!(
                "mail target `{handle}` is not a resident"
            )))
        } else {
            Err(CoreError::Invalid(format!(
                "resident `{handle}` is not terminal"
            )))
        }
    }

    /// Register the team root as the main actor so child mail (and quiescence) wake
    /// it. Idempotent: a second call for the same root is a no-op. The root is
    /// registered on the roster as `main` (transient mode — it is the root, not a
    /// resident subagent) if not already present.
    ///
    /// Callers must supply the already-resolved **team root** `AgentSpec`, can_spawn
    /// roster, and resource policy from the same captured [`TurnBinding`] as the
    /// triggering spawn (exact root stable `AgentName`, not the nested caller).
    /// `guidance` remains optional in-process-only triggering-turn text. Recovery
    /// invents nothing. Later `ensure_main` calls for the same root leave the first
    /// registration intact (first-wins).
    pub async fn ensure_main(
        &self,
        root: SessionId,
        agent: AgentSpec,
        resolved: (TurnBinding, Arc<[AgentDef]>, AgentResourcePolicy),
        actor_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(), CoreError> {
        let (binding, agents, resources) = resolved;
        let handle = self
            .engine
            .ensure_root_registered_for_actor(root, actor_claim)
            .await?;
        let team = self.team_for(root);
        let notify = Arc::new(Notify::new());
        let spawn = {
            let mut st = team.lock();
            if st.residents.contains_key(&root) {
                None
            } else {
                st.main_session = Some(root);
                st.residents.insert(
                    root,
                    SlotState {
                        handle,
                        agent,
                        binding,
                        // Required root activation context: always take the resolved path.
                        agents: Some(agents),
                        resources: Some(resources),
                        guidance,
                        sidecar_factory: None,
                        is_main: true,
                        status: SlotStatus::Idle,
                        pending: false,
                        initial_directive: None,
                        synth_pending: false,
                        claim: None,
                        kill_finalized: false,
                        cancel: team.cancel.child_token(),
                        stop_request: None,
                        cursor: 0, // main-as-actor injects child mail from its inbox
                        notify: notify.clone(),
                    },
                );
                Some(())
            }
        };
        if spawn.is_some() {
            tokio::spawn(resident_task(team.clone(), root, notify));
        }
        Ok(())
    }

    /// Spawn a brand-new resident under `parent`: create its session, assign a
    /// stable team-scoped handle, register it (mode = resident), announce it in the
    /// parent tree, and give it an initial wake so it runs its first turn on
    /// `directive`, then idles. Returns the new session + its handle.
    ///
    /// Non-blocking: this returns as soon as the resident is registered and armed;
    /// the caller (parent) does NOT wait for the resident's turn.
    ///
    /// `guidance` is the immutable triggering-turn Arc stored only in-process for
    /// every activation of this slot; it is not written to the resident definition.
    pub async fn spawn_resident(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String), CoreError> {
        let (binding, agents, resources, sidecar_factory) = resolved;
        let (root, parent_depth) = self.engine.session_lineage(parent).await?;
        let session = match parent_claim {
            Some(claim) => {
                self.engine
                    .create_for_actor(
                        claim,
                        CreateSession {
                            parent: Some(parent),
                            agent: agent.name.clone(),
                            model: agent.model.clone(),
                            workdir: agent.workdir.to_string_lossy().into_owned(),
                        },
                    )
                    .await?
            }
            None => {
                self.engine
                    .create(CreateSession {
                        parent: Some(parent),
                        agent: agent.name.clone(),
                        model: agent.model.clone(),
                        workdir: agent.workdir.to_string_lossy().into_owned(),
                    })
                    .await?
            }
        };
        let handle = self.assign_handle(root, agent.name.as_str()).await;
        // Announce in the parent tree (observable), then bind the handle + resident
        // mode in the team-root log.
        let member = MemberId::new();
        let description: String = directive.chars().take(80).collect();
        match parent_claim {
            Some(claim) => {
                self.engine
                    .commit_resident_mutation(
                        claim,
                        parent,
                        vec![Event::MemberSpawned {
                            session: parent,
                            member,
                            child: Some(session),
                            subagent_type: agent.name.clone(),
                            description,
                            depth: parent_depth.saturating_add(1),
                        }],
                    )
                    .await?;
            }
            None => {
                self.engine
                    .record_member_spawned(
                        parent,
                        member,
                        Some(session),
                        agent.name.clone(),
                        description,
                        parent_depth.saturating_add(1),
                    )
                    .await?;
            }
        }
        self.register_existing_resident_with_agents(
            root,
            session,
            handle.clone(),
            agent,
            ResidentRuntimeContext::Resolved {
                binding,
                agents,
                resources,
            },
            ResidentActivation {
                initial: Some(directive),
                guidance,
                sidecar_factory,
            },
        )
        .await?;
        Ok((session, handle))
    }

    /// Register an already-created `session` as a resident of team `root`, arm it,
    /// and (when `initial` is set) give it a first wake. Used by
    /// [`spawn_resident`](Self::spawn_resident); also the seam tests drive directly.
    pub async fn register_existing_resident(
        &self,
        root: SessionId,
        session: SessionId,
        handle: String,
        agent: AgentSpec,
        initial: Option<String>,
    ) -> Result<(), CoreError> {
        let binding = self.engine.bind_runtime(&agent.workdir)?;
        self.register_existing_resident_with_agents(
            root,
            session,
            handle,
            agent,
            ResidentRuntimeContext::Bound(binding),
            ResidentActivation {
                initial,
                guidance: None,
                sidecar_factory: None,
            },
        )
        .await
    }

    /// Register an already-created resident with a request-scoped sidecar factory.
    /// The factory remains opaque and in-memory; activation starts lazily on the
    /// resident's first owed turn.
    pub async fn register_existing_resident_with_sidecar(
        &self,
        root: SessionId,
        session: SessionId,
        handle: String,
        agent: AgentSpec,
        initial: Option<String>,
        sidecar_factory: Arc<dyn BoundSidecarFactory>,
    ) -> Result<(), CoreError> {
        let binding = self.engine.bind_runtime(&agent.workdir)?;
        self.register_existing_resident_with_agents(
            root,
            session,
            handle,
            agent,
            ResidentRuntimeContext::Bound(binding),
            ResidentActivation {
                initial,
                guidance: None,
                sidecar_factory: Some(sidecar_factory),
            },
        )
        .await
    }

    async fn register_existing_resident_with_agents(
        &self,
        root: SessionId,
        session: SessionId,
        handle: String,
        agent: AgentSpec,
        runtime: ResidentRuntimeContext,
        activation: ResidentActivation,
    ) -> Result<(), CoreError> {
        let (binding, agents, resources) = match runtime {
            ResidentRuntimeContext::Bound(binding) => (binding, None, None),
            ResidentRuntimeContext::Resolved {
                binding,
                agents,
                resources,
            } => (binding, Some(agents), Some(resources)),
        };
        let ResidentActivation {
            initial,
            guidance,
            sidecar_factory,
        } = activation;
        let claim = self
            .engine
            .store()
            .try_claim_new(session, self.owner_run_id)
            .await?;
        if let Err(registration_error) = self
            .engine
            .commit_resident_mutation(
                &claim,
                root,
                vec![Event::AgentRegistered {
                    session: root,
                    agent_session: session,
                    handle: handle.clone(),
                    parent: None,
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                }],
            )
            .await
        {
            if let Err(release_error) = self.engine.release_resident_actor_claim(&claim).await {
                return Err(CoreError::Invalid(format!(
                    "resident registration failed: {registration_error}; claim release failed: {release_error}"
                )));
            }
            return Err(registration_error);
        }
        let team = self.team_for(root);
        let notify = Arc::new(Notify::new());
        let initial = initial.filter(|directive| !directive.trim().is_empty());
        let has_initial = initial.is_some();
        {
            let mut st = team.lock();
            // New work exists (the initial directive), so a later quiescence fires.
            st.work_seq = st.work_seq.saturating_add(1);
            st.residents.insert(
                session,
                SlotState {
                    handle,
                    agent,
                    binding,
                    agents,
                    resources,
                    guidance,
                    sidecar_factory,
                    is_main: false,
                    status: SlotStatus::Idle,
                    pending: has_initial,
                    initial_directive: initial,
                    synth_pending: false,
                    claim: Some(claim),
                    kill_finalized: false,
                    cancel: team.cancel.child_token(),
                    stop_request: None,
                    cursor: 0,
                    notify: notify.clone(),
                },
            );
        }
        tokio::spawn(resident_task(team.clone(), session, notify.clone()));
        if has_initial {
            notify.notify_one();
        }
        Ok(())
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
        self.engine
            .store()
            .validate_actor_claim(&recovered.claim)
            .await?;
        let projection = self.engine.read_projection(root).await?;
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
        );
        let team = self.team_for(root);
        let notify = Arc::new(Notify::new());
        {
            let mut state = team.lock();
            state.residents.insert(
                recovered.claim.actor_id,
                SlotState {
                    handle,
                    agent,
                    binding,
                    agents: Some(agents),
                    resources: Some(resources),
                    // Ephemeral guidance is not durable; recovery invents nothing.
                    guidance: None,
                    sidecar_factory,
                    is_main: false,
                    status: SlotStatus::Idle,
                    pending,
                    initial_directive: None,
                    synth_pending: false,
                    claim: Some(recovered.claim),
                    kill_finalized: false,
                    cancel: team.cancel.child_token(),
                    stop_request: None,
                    cursor: usize::try_from(cursor).unwrap_or(usize::MAX),
                    notify: notify.clone(),
                },
            );
        }
        tokio::spawn(resident_task(
            team,
            recovered.claim.actor_id,
            notify.clone(),
        ));
        if pending {
            notify.notify_one();
        }
        Ok(())
    }

    /// Assign the next `{type}-{ordinal}` handle for `agent_type` in team `root`,
    /// continuing the ordinal past every existing member of that type. Deterministic
    /// (roster + type only) for replay stability, mirroring `subagent::assign_handles`.
    async fn assign_handle(&self, root: SessionId, agent_type: &str) -> String {
        let roster = self
            .engine
            .read_projection(root)
            .await
            .map(|p| p.team.roster)
            .unwrap_or_default();
        let used = roster
            .values()
            .filter(|entry| entry.session != root && entry.agent_type.as_str() == agent_type)
            .count();
        format!("{agent_type}-{}", used + 1)
    }
}
