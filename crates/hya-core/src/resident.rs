//! Resident subagent lifecycle: long-lived, event-driven actors (ADR-0002).
//!
//! A [`ResidentSupervisor`] turns a subset of a team's sessions into **actors**
//! that are idle at *zero token cost* and woken only by inbound mail. It rides the
//! same [`EventBus`](crate::bus::EventBus) `MailSent` stream the mailbox already
//! publishes (Phase 3) — that is the wake seam — so nothing new polls.
//!
//! ## Guarantees (see the per-method docs for where each is enforced)
//! - **Zero idle cost.** Every resident parks on a [`Notify`]; the bus listener
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
    ActorClaim, Event, FinishReason, MailEndpoint, MemberId, MemberRunStatus, OwnerRunId,
    PartProjection, Role, RosterStatus, SessionId, SubagentMode, ToolPartState,
};
use hya_tool::AgentDef;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;
use crate::orchestrator::TeamBudget;
use crate::{AgentResourcePolicy, TurnBinding};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentRecovery {
    Idle,
    Queued {
        inbox_cursor: u64,
    },
    AbortedRunning {
        inbox_cursor: u64,
        queued_after: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResidentRecoveryReport {
    pub work: ResidentRecovery,
    pub aborted_operations: usize,
}

impl SessionEngine {
    pub async fn recover_resident_actor(
        &self,
        recovered: &hya_store::RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<ResidentRecoveryReport, CoreError> {
        let aborted_operations = self.abort_recovered_actor_operations(recovered).await?;
        let work = self.recover_resident_work(recovered, root, handle).await?;
        Ok(ResidentRecoveryReport {
            work,
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
        let projection = self.read_projection(root).await?;
        let entry = projection
            .team
            .roster
            .get(handle)
            .filter(|entry| entry.session == recovered.claim.actor_id && entry.mode.is_resident())
            .ok_or_else(|| CoreError::Invalid("resident recovery roster mismatch".to_string()))?;
        if let Some(work) = entry.resident_work {
            if work.epoch != recovered.previous_epoch {
                return Err(CoreError::Invalid(
                    "resident recovery work epoch mismatch".to_string(),
                ));
            }
            self.terminalize_recovered_resident_effects(&recovered.claim)
                .await?;
            self.commit_resident_mutation(
                &recovered.claim,
                root,
                vec![Event::AgentActivityChanged {
                    session: root,
                    handle: handle.to_string(),
                    status: hya_proto::RosterStatus::Failed,
                    current_task: Some("aborted by resident recovery".to_string()),
                }],
            )
            .await?;
            let inbox_len = projection
                .team
                .inboxes
                .get(handle)
                .map_or(0, |inbox| inbox.len() as u64);
            return Ok(ResidentRecovery::AbortedRunning {
                inbox_cursor: work.inbox_through,
                queued_after: inbox_len > work.inbox_through,
            });
        }
        self.store().validate_actor_claim(&recovered.claim).await?;
        let inbox_len = projection
            .team
            .inboxes
            .get(handle)
            .map_or(0, |inbox| inbox.len() as u64);
        let actor_projection = self.read_projection(recovered.claim.actor_id).await?;
        let pending_user_turn = actor_projection
            .session
            .messages
            .last()
            .is_some_and(|message| message.role == hya_proto::Role::User);
        if inbox_len > entry.resident_cursor
            || (pending_user_turn && entry.status == RosterStatus::Idle)
        {
            Ok(ResidentRecovery::Queued {
                inbox_cursor: entry.resident_cursor,
            })
        } else {
            Ok(ResidentRecovery::Idle)
        }
    }

    async fn terminalize_recovered_resident_effects(
        &self,
        claim: &ActorClaim,
    ) -> Result<(), CoreError> {
        let projection = self.read_projection(claim.actor_id).await?;
        let mut events = Vec::new();
        for member in &projection.session.members {
            if matches!(
                member.status,
                MemberRunStatus::Spawning | MemberRunStatus::Running
            ) {
                events.push(Event::MemberFinished {
                    session: claim.actor_id,
                    member: member.member,
                    status: MemberRunStatus::Cancelled,
                    summary: "aborted by resident recovery".to_string(),
                    child: member.child,
                });
            }
        }
        for message in &projection.session.messages {
            if message.role != Role::Assistant || message.finish.is_some() {
                continue;
            }
            for part in &message.parts {
                if let PartProjection::Tool {
                    id,
                    call,
                    state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                    ..
                } = part
                {
                    events.push(Event::ToolError {
                        session: claim.actor_id,
                        message: message.id,
                        part: *id,
                        call: *call,
                        value: Some(serde_json::json!({
                            "code": "STALE_ACTOR_CLAIM",
                        })),
                        message_text: "aborted by resident recovery".to_string(),
                    });
                }
            }
            events.push(Event::MessageFinished {
                session: claim.actor_id,
                message: message.id,
                role: Role::Assistant,
                finish: FinishReason::Cancelled,
                tokens: None,
            });
        }
        if !events.is_empty() {
            self.commit_resident_mutation(claim, claim.actor_id, events)
                .await?;
        }
        Ok(())
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
    is_main: bool,
    status: SlotStatus,
    /// New mail has arrived and a turn is owed.
    pending: bool,
    /// (Main only) a synthesis directive is owed on the next turn.
    synth_pending: bool,
    /// Present for resident subagents; the main/root actor remains transient.
    claim: Option<ActorClaim>,
    /// How many of this handle's inbox messages have already been injected.
    cursor: usize,
    notify: Arc<Notify>,
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
}

/// Everything one turn needs, snapshotted atomically under the team lock so the
/// (long, unlocked) turn runs against a stable plan.
struct RunPlan {
    agent: AgentSpec,
    binding: TurnBinding,
    agents: Option<Arc<[AgentDef]>>,
    resources: Option<AgentResourcePolicy>,
    guidance: Option<Arc<str>>,
    handle: String,
    is_main: bool,
    /// Inbox messages before this index are already injected.
    cursor: usize,
    /// (Main only) inject the synthesis directive before the turn.
    synth: bool,
    claim: Option<ActorClaim>,
}

enum ResidentRuntimeContext {
    Bound(TurnBinding),
    Resolved {
        binding: TurnBinding,
        agents: Arc<[AgentDef]>,
        resources: AgentResourcePolicy,
    },
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
    /// The team was killed; the resident task must exit. `killed_now` is set for
    /// the single caller that observed the transition, so kill side-effects run once.
    Stop { killed_now: bool },
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
        if st.killed {
            return Action::Stop { killed_now: false };
        }
        let Some(slot) = st.residents.get_mut(&session) else {
            return Action::Stop { killed_now: false };
        };
        if slot.has_work() {
            let synth = slot.synth_pending;
            let claim = slot.claim;
            let cursor = slot.cursor;
            let agent = slot.agent.clone();
            let binding = slot.binding.clone();
            let agents = slot.agents.clone();
            let resources = slot.resources.clone();
            let guidance = slot.guidance.clone();
            let handle = slot.handle.clone();
            let is_main = slot.is_main;
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
                handle,
                is_main,
                cursor,
                synth,
                claim,
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
                .values()
                .map(|slot| (slot.handle.clone(), slot.claim))
                .collect::<Vec<_>>();
            (already, residents)
        };
        if already {
            return;
        }
        self.emit_kill(&residents, reason).await;
    }

    /// Emit the terminal `Failed` roster activity + release the team's budget
    /// counters. Separated so both kill paths (turn budget, message budget) share it.
    async fn emit_kill(&self, residents: &[(String, Option<ActorClaim>)], reason: &str) {
        for (handle, claim) in residents {
            let _ = self
                .record_activity(
                    claim.as_ref(),
                    handle.clone(),
                    RosterStatus::Failed,
                    Some(reason.to_string()),
                )
                .await;
            if let Some(claim) = claim {
                let _ = self.engine.release_resident_actor_claim(claim).await;
            }
        }
        if let Some(gov) = self.engine.governor() {
            gov.release_team(self.root);
        }
    }

    /// Run exactly one turn for `session`: inject the initial directive (once), the
    /// synthesis directive (main, on quiescence), and every inbox message since the
    /// cursor, then advance the cursor and stream one turn. All of this is coalesced
    /// into a single turn — many queued messages produce one turn, never several.
    async fn run_one_turn(&self, session: SessionId, plan: RunPlan) -> Result<(), CoreError> {
        let RunPlan {
            agent,
            binding,
            agents,
            resources,
            guidance,
            handle,
            is_main,
            cursor,
            synth,
            claim,
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
                    .run_resolved_turn_for_actor(
                        session,
                        &agent,
                        (binding, agents, resources),
                        claim,
                        self.cancel.child_token(),
                        guidance,
                    )
                    .await?;
            }
            (None, Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn(
                        session,
                        &agent,
                        (binding, agents, resources),
                        self.cancel.child_token(),
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
                        self.cancel.child_token(),
                        guidance,
                    )
                    .await?;
            }
            (None, _, _) => {
                self.engine
                    .run_bound_turn(
                        session,
                        &agent,
                        binding,
                        self.cancel.child_token(),
                        guidance,
                    )
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
    loop {
        notify.notified().await;
        loop {
            match team.next_action(session) {
                Action::Run(plan) => {
                    let handle = plan.handle.clone();
                    let claim = plan.claim;
                    match team.run_one_turn(session, *plan).await {
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
                                    .values()
                                    .map(|slot| (slot.handle.clone(), slot.claim))
                                    .collect::<Vec<_>>(),
                                st.kill_reason.clone().unwrap_or_default(),
                            )
                        };
                        team.emit_kill(&residents, &reason).await;
                    }
                    return;
                }
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
                        busy: 0,
                        killed: false,
                        work_seq: 0,
                        last_synth_work_seq: u64::MAX,
                        main_session: None,
                        kill_reason: None,
                    }),
                })
            })
            .clone()
    }

    /// The team-wide cancellation token for `root`, if the team is tracked. Exposed
    /// so the runtime/TUI can observe or force a team shutdown.
    #[must_use]
    pub fn team_cancel(&self, root: SessionId) -> Option<CancellationToken> {
        self.teams().get(&root).map(|team| team.cancel.clone())
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
                        is_main: true,
                        status: SlotStatus::Idle,
                        pending: false,
                        synth_pending: false,
                        claim: None,
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
        resolved: (TurnBinding, Arc<[AgentDef]>, AgentResourcePolicy),
        directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String), CoreError> {
        let (binding, agents, resources) = resolved;
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
            (Some(directive), guidance),
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
            (initial, None),
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
        activation: (Option<String>, Option<Arc<str>>),
    ) -> Result<(), CoreError> {
        let (binding, agents, resources) = match runtime {
            ResidentRuntimeContext::Bound(binding) => (binding, None, None),
            ResidentRuntimeContext::Resolved {
                binding,
                agents,
                resources,
            } => (binding, Some(agents), Some(resources)),
        };
        let (initial, guidance) = activation;
        let claim = self
            .engine
            .store()
            .try_claim_new(session, self.owner_run_id)
            .await?;
        self.engine
            .commit_resident_mutation(
                &claim,
                root,
                vec![Event::AgentRegistered {
                    session: root,
                    agent_session: session,
                    handle: handle.clone(),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                }],
            )
            .await?;
        let team = self.team_for(root);
        let notify = Arc::new(Notify::new());
        let has_initial = initial.as_ref().is_some_and(|d| !d.trim().is_empty());
        if let Some(initial) = initial.filter(|directive| !directive.trim().is_empty()) {
            self.engine
                .admit_user_prompt_for_actor(&claim, session, initial)
                .await?;
        }
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
                    is_main: false,
                    status: SlotStatus::Idle,
                    pending: has_initial,
                    synth_pending: false,
                    claim: Some(claim),
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

    pub async fn register_recovered_resident(
        &self,
        root: SessionId,
        handle: String,
        agent: AgentSpec,
        binding: TurnBinding,
        recovered: hya_store::RecoveredActorClaim,
        disposition: ResidentRecovery,
    ) -> Result<(), CoreError> {
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
                    agents: None,
                    resources: None,
                    // Ephemeral guidance is not durable; recovery invents nothing.
                    guidance: None,
                    is_main: false,
                    status: SlotStatus::Idle,
                    pending,
                    synth_pending: false,
                    claim: Some(recovered.claim),
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
