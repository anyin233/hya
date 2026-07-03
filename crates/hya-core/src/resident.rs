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

use hya_proto::{Event, MailEndpoint, MemberId, RosterStatus, SessionId, SubagentMode};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;
use crate::orchestrator::TeamBudget;

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
    is_main: bool,
    status: SlotStatus,
    /// New mail has arrived and a turn is owed.
    pending: bool,
    /// (Main only) a synthesis directive is owed on the next turn.
    synth_pending: bool,
    /// A one-shot initial directive to inject on the very first turn.
    initial: Option<String>,
    /// How many of this handle's inbox messages have already been injected.
    cursor: usize,
    notify: Arc<Notify>,
}

impl SlotState {
    /// Whether this slot owes a turn (mail, a synthesis directive, or its initial
    /// directive). The single source of truth for "is there work?" under the lock.
    fn has_work(&self) -> bool {
        self.pending || self.synth_pending || self.initial.is_some()
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
    handle: String,
    is_main: bool,
    /// Inbox messages before this index are already injected.
    cursor: usize,
    /// (Main only) inject the synthesis directive before the turn.
    synth: bool,
    /// One-shot initial directive to inject on the first turn.
    initial: Option<String>,
}

/// What a resident task should do next, decided atomically under the team lock.
enum Action {
    /// Run exactly one turn per the snapshotted [`RunPlan`].
    Run(RunPlan),
    /// No work owed; the slot just transitioned to idle (`became_idle` gates the
    /// roster activity emission so it fires once per idle transition).
    Idle { handle: String, became_idle: bool },
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
            let initial = slot.initial.take();
            let cursor = slot.cursor;
            let agent = slot.agent.clone();
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
            Action::Run(RunPlan {
                agent,
                handle,
                is_main,
                cursor,
                synth,
                initial,
            })
        } else {
            let handle = slot.handle.clone();
            let became_idle = slot.status == SlotStatus::Busy;
            if became_idle {
                slot.status = SlotStatus::Idle;
                st.busy = st.busy.saturating_sub(1);
                self.maybe_fire_quiescence(&mut st);
            }
            Action::Idle {
                handle,
                became_idle,
            }
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
        let (already, handles) = {
            let mut st = self.lock();
            let already = st.killed;
            self.kill_locked(&mut st, reason);
            let handles: Vec<String> = st.residents.values().map(|s| s.handle.clone()).collect();
            (already, handles)
        };
        if already {
            return;
        }
        self.emit_kill(&handles, reason).await;
    }

    /// Emit the terminal `Failed` roster activity + release the team's budget
    /// counters. Separated so both kill paths (turn budget, message budget) share it.
    async fn emit_kill(&self, handles: &[String], reason: &str) {
        for handle in handles {
            let _ = self
                .engine
                .record_agent_activity(
                    self.root,
                    handle.clone(),
                    RosterStatus::Failed,
                    Some(reason.to_string()),
                )
                .await;
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
            handle,
            is_main,
            cursor,
            synth,
            initial,
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
        let _ = self
            .engine
            .record_agent_activity(
                self.root,
                handle.clone(),
                RosterStatus::Busy,
                Some(task_label),
            )
            .await;

        if let Some(directive) = initial
            && !directive.trim().is_empty()
        {
            self.engine.admit_user_prompt(session, directive).await?;
        }
        if synth && is_main {
            self.engine
                .inject_system_message(session, SYNTHESIS_DIRECTIVE.to_string())
                .await?;
        }
        for (from, body) in &new_mail {
            self.engine
                .admit_user_prompt(session, format!("[mail from {from}] {body}"))
                .await?;
        }
        // Advance the cursor so a follow-up turn never re-injects the same mail.
        {
            let mut st = self.lock();
            if let Some(slot) = st.residents.get_mut(&session) {
                slot.cursor = slot.cursor.max(inbox_len);
            }
        }
        // Exactly one turn, under the team-wide cancel so a budget kill stops it.
        self.engine
            .run_turn(session, &agent, self.cancel.child_token())
            .await?;
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
                    if let Err(err) = team.run_one_turn(session, plan).await {
                        // A turn error must not wedge the actor: record it and let
                        // the loop re-decide (it will idle if nothing else is owed).
                        let _ = team
                            .engine
                            .record_agent_activity(
                                team.root,
                                handle,
                                RosterStatus::Failed,
                                Some(format!("turn error: {err}")),
                            )
                            .await;
                    }
                }
                Action::Idle {
                    handle,
                    became_idle,
                } => {
                    if became_idle {
                        let _ = team
                            .engine
                            .record_agent_activity(team.root, handle, RosterStatus::Idle, None)
                            .await;
                    }
                    break;
                }
                Action::Stop { killed_now } => {
                    if killed_now {
                        let (handles, reason) = {
                            let st = team.lock();
                            (
                                st.residents
                                    .values()
                                    .map(|s| s.handle.clone())
                                    .collect::<Vec<_>>(),
                                st.kill_reason.clone().unwrap_or_default(),
                            )
                        };
                        team.emit_kill(&handles, &reason).await;
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
        let rx = engine.bus().subscribe();
        let supervisor = Arc::new(Self {
            engine,
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
    pub async fn ensure_main(&self, root: SessionId, agent: AgentSpec) -> Result<(), CoreError> {
        let handle = self.engine.ensure_root_registered(root).await?;
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
                        is_main: true,
                        status: SlotStatus::Idle,
                        pending: false,
                        synth_pending: false,
                        initial: None,
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
    pub async fn spawn_resident(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        directive: String,
    ) -> Result<(SessionId, String), CoreError> {
        let (root, parent_depth) = self.engine.session_lineage(parent).await?;
        let session = self
            .engine
            .create(CreateSession {
                parent: Some(parent),
                agent: agent.name.clone(),
                model: agent.model.clone(),
                workdir: agent.workdir.to_string_lossy().into_owned(),
            })
            .await?;
        let handle = self.assign_handle(root, agent.name.as_str()).await;
        // Announce in the parent tree (observable), then bind the handle + resident
        // mode in the team-root log.
        let member = MemberId::new();
        let description: String = directive.chars().take(80).collect();
        let _ = self
            .engine
            .record_member_spawned(
                parent,
                member,
                Some(session),
                agent.name.clone(),
                description,
                parent_depth.saturating_add(1),
            )
            .await;
        self.register_existing_resident(root, session, handle.clone(), agent, Some(directive))
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
        self.engine
            .record_agent_registered(
                root,
                session,
                handle.clone(),
                agent.name.clone(),
                SubagentMode::Resident,
            )
            .await?;
        let team = self.team_for(root);
        let notify = Arc::new(Notify::new());
        let has_initial = initial.as_ref().is_some_and(|d| !d.trim().is_empty());
        {
            let mut st = team.lock();
            // New work exists (the initial directive), so a later quiescence fires.
            st.work_seq = st.work_seq.saturating_add(1);
            st.residents.insert(
                session,
                SlotState {
                    handle,
                    agent,
                    is_main: false,
                    status: SlotStatus::Idle,
                    pending: false,
                    synth_pending: false,
                    initial,
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
