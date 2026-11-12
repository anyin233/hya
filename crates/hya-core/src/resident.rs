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
//! - **Single active turn per session.** A slot claims its session's engine
//!   turn lease under the team lock before running. A session whose turn is
//!   already active (e.g. the lead's own `exec`/`serve` turn while it is the
//!   main actor) is busy: its wake stays queued and quiescence is held back
//!   until that turn's lease is released, when the supervisor (the engine's
//!   `TurnBoundaryObserver`) delivers the deferred work.
//! - **Runaway kill.** Per-team turn and message counters live on the team;
//!   exceeding their configured limits cancels its episodes and harness lead turn.
//!
//! ## Concurrency model
//! All accounting (busy count, per-slot status/pending, quiescence, kill) lives
//! behind ONE `std::sync::Mutex<TeamState>` per team. Critical sections are short
//! and never `.await`; the long work (reading the projection, injecting mail,
//! running a turn) happens *outside* the lock. Because the decision to go idle and
//! the busy→0 quiescence check happen in the same locked section that observes
//! "no pending work", quiescence can never fire while a turn is running or mail is
//! queued, and it can never hang (the last resident to idle always runs the check).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use hya_bundle::{ChannelCapability, ChannelParticipantRole, ChannelTemplateKind};
use hya_proto::{
    ActorClaim, ActorEpoch, ArchiveReason, ChannelKind, Event, FinishCause, MailEndpoint, MemberId,
    MemberRunStatus, ModelRef, OwnerRunId, ReportOutcome, RosterStatus, SessionId, SubagentMode,
    ToolCallId, scope,
};
use hya_tool::{AgentDef, ArchiveReceipt, ResolvedTool};
use tokio::sync::{Notify, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::engine::{
    AgentSpec, ArchiveReviver, DRAIN_DEADLINE, SessionEngine, TurnBoundaryObserver,
    TurnDrainReport, TurnLease, TurnRequestContext,
};
use crate::error::CoreError;
use crate::hooks::{
    AgentSpawnInput, HookDispatcher, SessionLifecycleInput, scope_activation_hooks,
};
use crate::sidecar::{BoundSidecarFactory, SidecarHandle, SidecarStart};
use crate::workflow::WorkflowTurnRoute;
use crate::{AgentResourcePolicy, ChannelPolicy, TurnBinding};

/// Runtime pieces needed to resume a resident after process restart.
pub type ResolvedResidentRuntime = (
    TurnBinding,
    Arc<[AgentDef]>,
    AgentResourcePolicy,
    Option<Arc<dyn BoundSidecarFactory>>,
);

/// Initial activation policy for the shared resident registration path.
enum ResidentRegistration {
    /// Register and immediately arm the supplied directive.
    Armed(String),
    /// Register without an implicit first turn.
    Parked(String),
}

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

/// Minimum spacing between harness heartbeats for one busy slot (ADR-0002
/// liveness). A busy child that is really progressing emits fresh engine
/// events; the supervisor folds those observations into `AgentHeartbeat` at
/// most this often so the parent can tell progress from a stall.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Current wall-clock as non-zero epoch milliseconds for a heartbeat value.
fn heartbeat_now_ms() -> u64 {
    u64::try_from(hya_proto::now_millis()).unwrap_or(0).max(1)
}

/// Per-slot activity, tracked in-memory (the durable mirror is `RosterStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotStatus {
    Idle,
    Busy,
}

/// A report accepted from the `report` tool, executed once the actor is at
/// rest (ADR-0015): the gate was checked at call time; the archive runs after
/// the current turn ends and re-checks.
#[derive(Clone)]
struct PendingReport {
    outcome: ReportOutcome,
    report: String,
}

/// One resident (or the main actor) inside a team.
struct Episode {
    handle: String,
    agent: AgentSpec,
    /// Immutable runtime snapshot retained for every activation of this slot.
    binding: TurnBinding,
    agents: Option<Arc<[AgentDef]>>,
    resources: Option<AgentResourcePolicy>,
    /// In-process activation guidance. Workflow actors replace this only while
    /// idle, before durable mail wakes the next Stage. Recovery invents none.
    guidance: Option<Arc<str>>,
    /// Fixed request-local route for Workflow resident activations.
    route: Option<WorkflowTurnRoute>,
    /// Opaque request-scoped sidecar factory, retained only in memory.
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    is_main: bool,
    status: SlotStatus,
    /// New mail has arrived and a turn is owed.
    pending: bool,
    /// (Main only) a synthesis directive is owed on the next turn.
    synth_pending: bool,
    /// Present for resident subagents; the main/root actor remains transient.
    claim: Option<ActorClaim>,
    /// Slot-local cancellation, inherited from the team's cancellation token.
    cancel: CancellationToken,
    ending: Option<Ending>,
    done: EpisodeCompletion,
    /// The committed initial directive is an input even when no mail is owed.
    initial_prompt: Option<hya_proto::MessageId>,
    /// Accepted-but-not-yet-executed terminal report (ADR-0015).
    report: Option<PendingReport>,
    /// The user cancelled this slot's or an ancestor's turn: the harness
    /// starts no turn on it (owed work stays queued) until the user acts on
    /// it again with a prompt or a steer.
    user_stopped: bool,
    /// How many of this handle's inbox messages have already been injected.
    cursor: usize,
    /// Epoch-ms of the last engine activity observed on the bus for this
    /// actor. Harness truth only; drives heartbeat values, never scheduling.
    last_activity: Option<u64>,
    /// When the harness last emitted an `AgentHeartbeat` for this slot; the
    /// 30-second throttle window anchors here.
    last_heartbeat_emit: Option<Instant>,
    notify: Arc<Notify>,
}

/// The first terminal request owns the ending; later callers only wait.
#[derive(Clone)]
enum Ending {
    Reported(PendingReport),
    Failed(String),
    Archived(String),
    Killed(String),
    Teardown,
    Shutdown,
}

impl Ending {
    fn cause(&self) -> FinishCause {
        match self {
            Self::Archived(_) | Self::Teardown => FinishCause::Archived,
            Self::Shutdown => FinishCause::Shutdown,
            Self::Killed(_) => FinishCause::Other,
            Self::Failed(_) => FinishCause::Other,
            Self::Reported(_) => FinishCause::UserCancel,
        }
    }

    fn forced_fields(&self) -> (ArchiveReason, MemberRunStatus, String) {
        match self {
            Self::Archived(note) => (
                ArchiveReason::ArchivedByParent,
                MemberRunStatus::Cancelled,
                note.clone(),
            ),
            Self::Killed(note) => (ArchiveReason::Killed, MemberRunStatus::Failed, note.clone()),
            Self::Teardown => (
                ArchiveReason::RootTeardown,
                MemberRunStatus::Cancelled,
                "root turn teardown".into(),
            ),
            Self::Shutdown => (
                ArchiveReason::Shutdown,
                MemberRunStatus::Cancelled,
                "stopped: shutdown".into(),
            ),
            Self::Reported(_) | Self::Failed(_) => unreachable!("report is not a forced ending"),
        }
    }
}

impl Episode {
    fn request_end(
        &mut self,
        engine: &SessionEngine,
        session: SessionId,
        ending: Ending,
    ) -> watch::Receiver<Option<Result<(), CoreError>>> {
        let done = self.done.subscribe();
        if self.ending.is_none() {
            if !self.is_main {
                engine.cancel_turn(session, ending.cause());
            }
            self.ending = Some(ending);
            self.report = None;
            self.cancel.cancel();
            self.notify.notify_one();
        }
        done
    }

    /// Whether this slot owes a turn (mail, a synthesis directive, or its initial
    /// directive). The single source of truth for "is there work?" under the lock.
    fn has_work(&self) -> bool {
        self.ending.is_none() && (self.pending || self.synth_pending)
    }
}

#[derive(Clone)]
enum TeamPhase {
    Live,
    Killed(Ending),
}

impl TeamPhase {
    fn killed(&self) -> bool {
        matches!(self, Self::Killed(_))
    }
}

type EpisodeDone = watch::Receiver<Option<Result<(), CoreError>>>;
type EpisodeCompletion = watch::Sender<Option<Result<(), CoreError>>>;

/// The mutable, lock-guarded state of one team.
struct TeamState {
    residents: HashMap<SessionId, Episode>,
    /// Number of non-idle residents (incl. main). Quiescence = this is 0.
    busy: usize,
    phase: TeamPhase,
    turns_used: u64,
    messages_used: u64,
    /// Monotonic counter bumped on every unit of new work (mail, registration).
    /// Quiescence re-fires only when this advances past the last synthesis.
    work_seq: u64,
    /// `work_seq` captured at the last synthesis wake. Initialized to `u64::MAX`
    /// so the FIRST quiescence always fires (any real `work_seq` differs from it).
    last_synth_work_seq: u64,
    main_session: Option<SessionId>,
    /// The lead's (main actor's) last turn failed. While set the harness
    /// starts no main turn on its own — no quiescence synthesis, no mail
    /// wake — until a turn on the lead starts from outside the supervisor
    /// (the user resumes it). Queued mail stays in the lead's inbox.
    lead_failed: bool,
}

#[derive(Clone)]
struct MainActivation {
    agent: AgentSpec,
    binding: TurnBinding,
    agents: Arc<[AgentDef]>,
    resources: AgentResourcePolicy,
    guidance: Option<Arc<str>>,
}

/// Everything one turn needs, snapshotted atomically under the team lock so the
/// (long, unlocked) turn runs against a stable plan.
struct RunPlan {
    agent: AgentSpec,
    binding: TurnBinding,
    agents: Option<Arc<[AgentDef]>>,
    resources: Option<AgentResourcePolicy>,
    guidance: Option<Arc<str>>,
    route: Option<WorkflowTurnRoute>,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    handle: String,
    is_main: bool,
    /// Inbox messages before this index are already injected.
    cursor: usize,
    /// (Main only) inject the synthesis directive before the turn.
    synth: bool,
    claim: Option<ActorClaim>,
    cancel: CancellationToken,
    initial_prompt: Option<hya_proto::MessageId>,
    /// The session's single-active-turn claim, taken atomically with the
    /// decision to run and held until the turn ends.
    lease: TurnLease,
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
    events: Vec<Event>,
    parent_claim: Option<ActorClaim>,
    initial: Option<String>,
    guidance: Option<Arc<str>>,
    route: Option<WorkflowTurnRoute>,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
}

/// What a `task` tool call contributes to a resident spawn.
///
/// Spawns without one (engine-initiated residents, Workflow Stages) name the
/// handle after the spec's agent, describe the member row with the start of
/// the directive, and record no `MemberSpawned.tool_call`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskSpawnOrigin {
    /// The agent id the call named (`subagent_type`, resolved); the handle
    /// prefix is its sanitized form.
    pub subagent_type: String,
    /// The call's short description, shown on the member row instead of the
    /// prompt. Empty falls back to the start of the directive.
    pub description: String,
    /// The spawning `task` call, recorded on `MemberSpawned.tool_call` so a
    /// client links the member row to its tool card.
    pub tool_call: Option<ToolCallId>,
}

/// Owned registration context for one resident creation operation.
struct ResidentSpawnContext {
    registration: ResidentRegistration,
    parent_claim: Option<ActorClaim>,
    guidance: Option<Arc<str>>,
    /// The `task` call behind the spawn, when there is one.
    task: Option<TaskSpawnOrigin>,
}

impl ResidentSpawnContext {
    fn new(
        registration: ResidentRegistration,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Self {
        Self {
            registration,
            parent_claim: parent_claim.copied(),
            guidance,
            task: None,
        }
    }

    fn with_task(mut self, task: TaskSpawnOrigin) -> Self {
        self.task = Some(task);
        self
    }
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
    /// The episode task runs the requested ending after its current turn.
    End(Ending),
}

/// A single team's actor group: its residents, budgets, and cancellation.
struct TeamActor {
    root: SessionId,
    engine: Arc<SessionEngine>,
    cancel: CancellationToken,
    state: Mutex<TeamState>,
    supervisor: Weak<ResidentSupervisor>,
}

impl TeamActor {
    fn new(
        root: SessionId,
        engine: Arc<SessionEngine>,
        supervisor: Weak<ResidentSupervisor>,
    ) -> Self {
        Self {
            root,
            engine,
            cancel: CancellationToken::new(),
            supervisor,
            state: Mutex::new(TeamState {
                residents: HashMap::new(),
                busy: 0,
                phase: TeamPhase::Live,
                turns_used: 0,
                messages_used: 0,
                work_seq: 0,
                last_synth_work_seq: u64::MAX,
                main_session: None,
                lead_failed: false,
            }),
        }
    }

    /// Main is installed before a fresh team is published to the supervisor.
    fn install_main(self: &Arc<Self>, context: MainActivation) {
        let notify = Arc::new(Notify::new());
        let cancel = self.cancel.child_token();
        {
            let mut state = self.lock();
            if state.residents.contains_key(&self.root) {
                return;
            }
            state.main_session = Some(self.root);
            state.residents.insert(
                self.root,
                Episode {
                    handle: scope::ROOT_HANDLE.into(),
                    agent: context.agent,
                    binding: context.binding,
                    agents: Some(context.agents),
                    resources: Some(context.resources),
                    guidance: context.guidance,
                    route: None,
                    sidecar_factory: None,
                    is_main: true,
                    status: SlotStatus::Idle,
                    pending: false,
                    report: None,
                    user_stopped: false,
                    synth_pending: false,
                    claim: None,
                    cancel: cancel.clone(),
                    ending: None,
                    done: watch::channel(None).0,
                    initial_prompt: None,
                    cursor: 0,
                    last_activity: None,
                    last_heartbeat_emit: None,
                    notify: notify.clone(),
                },
            );
        }
        tokio::spawn(episode_task(
            self.clone(),
            self.root,
            None,
            notify,
            cancel,
            None,
        ));
    }

    fn install_episode(
        self: &Arc<Self>,
        session: SessionId,
        mut slot: Episode,
        init: Option<EpisodeInit>,
    ) {
        let claim = slot.claim;
        let cancel = slot.cancel.clone();
        let notify = slot.notify.clone();
        let pending = slot.has_work();
        {
            let mut state = self.lock();
            assert!(
                !state.residents.contains_key(&session),
                "episode installed twice"
            );
            if let TeamPhase::Killed(ending) = &state.phase {
                slot.request_end(&self.engine, session, ending.clone());
            }
            state.work_seq = state.work_seq.saturating_add(1);
            state.residents.insert(session, slot);
        }
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(session, self.clone());
        }
        tokio::spawn(episode_task(
            self.clone(),
            session,
            claim,
            notify.clone(),
            cancel,
            init,
        ));
        if pending {
            notify.notify_one();
        }
    }

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

    /// Append one harness heartbeat to the team-root log. A failed append is a
    /// dropped liveness hint, never an error path: a fenced claim racing a
    /// stop/kill simply loses that heartbeat.
    async fn record_heartbeat(&self, claim: Option<&ActorClaim>, handle: &str, heartbeat_ms: u64) {
        let event = Event::AgentHeartbeat {
            session: self.root,
            handle: handle.to_string(),
            heartbeat_ms,
        };
        let _ = self.engine.emit_for_actor(claim, self.root, event).await;
    }

    /// Record that `session`'s actor produced real engine work (an event the
    /// supervisor observed on the bus, owned by that session) and emit a
    /// throttled `AgentHeartbeat` while the slot is busy.
    async fn note_activity(&self, session: SessionId) {
        let Some((handle, claim, heartbeat_ms)) = self.touch_activity(session) else {
            return;
        };
        self.record_heartbeat(claim.as_ref(), &handle, heartbeat_ms)
            .await;
    }

    /// Lock-scoped activity accounting: stamps the slot's last activity and
    /// decides — busy state plus the 30-second window — whether this
    /// observation is the one that emits. The window is consumed here so at
    /// most one heartbeat lands per interval per slot.
    fn touch_activity(&self, session: SessionId) -> Option<(String, Option<ActorClaim>, u64)> {
        let mut st = self.lock();
        let slot = st.residents.get_mut(&session)?;
        let heartbeat_ms = heartbeat_now_ms();
        slot.last_activity = Some(heartbeat_ms);
        if slot.status != SlotStatus::Busy {
            return None;
        }
        let due = slot
            .last_heartbeat_emit
            .is_none_or(|last| Instant::now().duration_since(last) >= HEARTBEAT_INTERVAL);
        if !due {
            return None;
        }
        slot.last_heartbeat_emit = Some(Instant::now());
        Some((slot.handle.clone(), slot.claim, heartbeat_ms))
    }

    /// Turn-start heartbeat: stamped "now" while the claim owns the slot — work is
    /// beginning this instant. It also re-arms the throttle window so mid-turn
    /// bus events inside the window stay quiet.
    async fn heartbeat_turn_start(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        handle: &str,
    ) {
        let heartbeat_ms = heartbeat_now_ms();
        {
            let mut st = self.lock();
            let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim.as_ref() == claim)
            else {
                return;
            };
            slot.last_activity = Some(heartbeat_ms);
            slot.last_heartbeat_emit = Some(Instant::now());
        }
        self.record_heartbeat(claim, handle, heartbeat_ms).await;
    }

    /// Turn-finish heartbeat: stamped with the owning slot's LAST
    /// observed activity rather than "now" — a turn that went quiet long before
    /// finishing must not overstate liveness, and the max-fold makes an older
    /// stamp safe. Re-arms the throttle window for the next turn.
    async fn heartbeat_turn_finish(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        handle: &str,
    ) {
        let now_ms = heartbeat_now_ms();
        let heartbeat_ms = {
            let mut st = self.lock();
            match st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim.as_ref() == claim)
            {
                Some(slot) => {
                    slot.last_heartbeat_emit = Some(Instant::now());
                    slot.last_activity.unwrap_or(now_ms)
                }
                None => return,
            }
        };
        self.record_heartbeat(claim, handle, heartbeat_ms).await;
    }

    fn owns_slot(&self, session: SessionId, claim: Option<&ActorClaim>) -> bool {
        self.lock()
            .residents
            .get(&session)
            .is_some_and(|slot| slot.claim.as_ref() == claim)
    }

    /// Decide atomically what this claim does next, charging the turn budget.
    async fn next_action(&self, session: SessionId, claim: Option<&ActorClaim>) -> Action {
        let mut st = self.lock();
        let lead_failed = st.lead_failed;
        let Some(slot) = st
            .residents
            .get_mut(&session)
            .filter(|slot| slot.claim.as_ref() == claim)
        else {
            unreachable!("episode task lost its slot");
        };
        if let Some(ending) = slot.ending.clone() {
            return Action::End(ending);
        }
        // A failed lead is never woken by the harness: its owed work (mail)
        // waits for the user to resume it.
        // Nor is a slot the user stopped: its work waits for the user too.
        if slot.has_work() && !(slot.is_main && lead_failed) && !slot.user_stopped {
            // Single active turn per session: when this session already has a
            // turn (e.g. the lead's own interactive turn while it is the main
            // actor), keep the owed work queued. The turn-boundary observer
            // re-wakes this slot when that turn's lease is released.
            let lease = match self.engine.try_begin_turn(session) {
                Ok(lease) => lease,
                Err(_) => {
                    let handle = slot.handle.clone();
                    let claim = slot.claim;
                    let became_idle = slot.status == SlotStatus::Busy;
                    if became_idle {
                        slot.status = SlotStatus::Idle;
                        st.busy = st.busy.saturating_sub(1);
                        self.maybe_fire_quiescence(&mut st);
                    }
                    return Action::Idle {
                        handle,
                        claim,
                        became_idle,
                    };
                }
            };
            let synth = slot.synth_pending;
            let initial_prompt = slot.initial_prompt;
            let claim = slot.claim;
            let cursor = slot.cursor;
            let agent = slot.agent.clone();
            let binding = slot.binding.clone();
            let agents = slot.agents.clone();
            let resources = slot.resources.clone();
            let guidance = slot.guidance.clone();
            let route = slot.route.clone();
            let sidecar_factory = slot.sidecar_factory.clone();
            let handle = slot.handle.clone();
            let is_main = slot.is_main;
            let cancel = slot.cancel.child_token();
            slot.pending = false;
            slot.synth_pending = false;
            // Reserve the start window before published registration events
            // can be observed as busy activity and race the boundary heartbeat.
            slot.last_heartbeat_emit = Some(Instant::now());
            if slot.status == SlotStatus::Idle {
                slot.status = SlotStatus::Busy;
                st.busy += 1;
            }
            Action::Run(Box::new(RunPlan {
                agent,
                binding,
                agents,
                resources,
                route,
                guidance,
                sidecar_factory,
                handle,
                is_main,
                cursor,
                synth,
                initial_prompt,
                claim,
                cancel,
                lease,
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

    fn finish_run(&self, session: SessionId, claim: Option<&ActorClaim>) {
        let mut state = self.lock();
        let has_work = match state
            .residents
            .get_mut(&session)
            .filter(|slot| slot.claim.as_ref() == claim)
        {
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
        drop(state);
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor.retire_team(self);
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
        if matches!(disposition, ResidentRecovery::AbortedRunning { .. }) {
            slot.initial_prompt = None;
        }
        if let Some(cursor) = cursor {
            slot.cursor = cursor;
        }
        slot.pending |= pending;
        Ok(())
    }

    fn remove_slot(&self, session: SessionId, claim: Option<&ActorClaim>) {
        {
            let mut state = self.lock();
            let std::collections::hash_map::Entry::Occupied(entry) = state.residents.entry(session)
            else {
                return;
            };
            if entry.get().claim.as_ref() != claim {
                return;
            }
            let slot = entry.remove();
            if slot.done.borrow().is_none() {
                slot.done.send_replace(Some(Ok(())));
            }
            if slot.status == SlotStatus::Busy {
                state.busy = state.busy.saturating_sub(1);
            }
            if state.main_session == Some(session) {
                state.main_session = None;
            }
        }
        self.forget_episode(session, claim);
    }

    fn forget_episode(&self, session: SessionId, claim: Option<&ActorClaim>) {
        if let Some(supervisor) = self.supervisor.upgrade() {
            let mut episodes = supervisor
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if episodes
                .get(&session)
                .is_some_and(|team| std::ptr::eq(team.as_ref(), self))
            {
                episodes.remove(&session);
            }
            drop(episodes);
            if claim.is_some() {
                supervisor.retire_team(self);
            }
        }
    }

    /// If the team just went fully idle with new work since the last synthesis,
    /// wake the main actor to synthesize. Pure in-memory + `Notify`, safe under
    /// the lock. Not firing when `work_seq` is unchanged is what guarantees
    /// termination: main's own synthesis (which produces no new work) leaves
    /// `work_seq` equal, so the next idle transition is a no-op (the team is done).
    fn maybe_fire_quiescence(&self, st: &mut TeamState) {
        // A failed lead gets no synthesis turn: the lead is dead until the
        // user resumes it (the synthesis re-fires after its next turn).
        if st.phase.killed() || st.busy != 0 || st.lead_failed {
            return;
        }
        // Nor does a lead the user stopped: the user, not the harness, says
        // what happens next (the synthesis re-fires after its next turn).
        if st
            .main_session
            .and_then(|main| st.residents.get(&main))
            .is_some_and(|slot| slot.user_stopped)
        {
            return;
        }
        // A member (usually the lead) whose own turn is still running is not
        // idle, even when the supervisor did not start that turn. Quiescence
        // is re-checked when that turn's lease is released.
        if st
            .residents
            .keys()
            .any(|session| self.engine.turn_active(*session))
        {
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

    /// In-turn steering (or an explicit read, or `wait`) consumed the lead's
    /// queued mail, as of the inbox snapshot taken after `mark` (the team's
    /// `work_seq` then). Do not replay that already-seen work as a quiescence
    /// synthesis wake when the lease ends. Work counted after `mark` was not in
    /// that snapshot: it keeps its wake and its synthesis.
    fn on_mail_consumed(&self, session: SessionId, mark: u64) {
        let mut st = self.lock();
        if st.main_session != Some(session) {
            return;
        }
        if st.work_seq == mark
            && let Some(slot) = st.residents.get_mut(&session)
        {
            slot.pending = false;
        }
        // Never move the synthesis mark backwards past a synthesis that
        // already fired (`u64::MAX`: none has yet).
        if st.last_synth_work_seq == u64::MAX || st.last_synth_work_seq < mark {
            st.last_synth_work_seq = mark;
        }
    }

    /// A turn on `session` ended (its lease was released). Deliver work that
    /// was deferred while it ran — mail or a synthesis owed to that slot — and
    /// re-check quiescence, which that turn was holding back.
    fn on_turn_released(&self, session: SessionId) {
        let mut st = self.lock();
        if st.phase.killed() {
            return;
        }
        if let Some(slot) = st.residents.get(&session)
            && slot.has_work()
        {
            slot.notify.notify_one();
        }
        self.maybe_fire_quiescence(&mut st);
    }

    /// The lead's turn failed: hold back every harness-started lead turn
    /// (synthesis, mail wakes) until the lead is resumed from outside.
    fn on_lead_turn_failed(&self, session: SessionId) {
        let mut st = self.lock();
        if st.main_session != Some(session) {
            return;
        }
        st.lead_failed = true;
        if let Some(slot) = st.residents.get_mut(&session) {
            slot.synth_pending = false;
        }
    }

    /// A turn on the lead started. While the lead is marked failed the
    /// supervisor starts none, so this is the user resuming it.
    fn on_lead_turn_started(&self, session: SessionId) {
        let mut st = self.lock();
        if st.main_session == Some(session) {
            st.lead_failed = false;
        }
    }

    /// The sessions of `session`'s slot and every slot below it: the whole
    /// team for the lead (or the root when it has no main slot).
    fn subtree(st: &TeamState, root: SessionId, session: SessionId) -> Vec<SessionId> {
        let handle = st
            .residents
            .get(&session)
            .filter(|slot| !slot.is_main && session != root)
            .map(|slot| slot.handle.clone());
        if handle.is_none() && session != root {
            return Vec::new();
        }
        st.residents
            .iter()
            .filter(|(_, slot)| {
                handle.as_deref().is_none_or(|handle| {
                    slot.handle == handle
                        || slot
                            .handle
                            .strip_prefix(handle)
                            .is_some_and(|rest| rest.starts_with(scope::PATH_SEPARATOR))
                })
            })
            .map(|(member, _)| *member)
            .collect()
    }

    /// The user cancelled `session`'s turn: stop its slot and every slot below
    /// it. Their queued mail wakes stay owed but held (a pending synthesis is
    /// dropped): the harness starts none of their turns until
    /// [`resume_for_user`](Self::resume_for_user) delivers them. Returns the
    /// stopped sessions, whose in-flight turns the caller cancels — none, and
    /// nothing is stopped, when neither `session` nor any slot below it is
    /// running or owes a turn (there is nothing to interrupt).
    fn stop_for_user(&self, session: SessionId) -> Vec<SessionId> {
        let mut st = self.lock();
        if st.phase.killed() {
            return Vec::new();
        }
        let stopped = Self::subtree(&st, self.root, session);
        let running = self.engine.turn_active(session)
            || stopped.iter().any(|member| {
                self.engine.turn_active(*member)
                    || st.residents.get(member).is_some_and(|slot| {
                        slot.status == SlotStatus::Busy || (slot.has_work() && !slot.user_stopped)
                    })
            });
        if !running {
            return Vec::new();
        }
        for member in &stopped {
            if let Some(slot) = st.residents.get_mut(member) {
                slot.user_stopped = true;
                slot.synth_pending = false;
            }
        }
        stopped
    }

    /// The user acted on `session` again (a prompt or a steer): lift the stop
    /// from its slot and every slot below it, waking those with mail queued
    /// before or during the stop.
    fn resume_for_user(&self, session: SessionId) {
        let mut st = self.lock();
        for member in Self::subtree(&st, self.root, session) {
            if let Some(slot) = st.residents.get_mut(&member)
                && std::mem::take(&mut slot.user_stopped)
                && slot.has_work()
            {
                slot.notify.notify_one();
            }
        }
    }

    /// Whether the user stopped the live slot at `handle`.
    fn handle_stopped_by_user(&self, handle: &str) -> bool {
        self.lock()
            .residents
            .values()
            .any(|slot| slot.handle == handle && slot.user_stopped)
    }

    /// Request every ending before publishing any terminal activity. The team
    /// leaves the live map immediately; episode owners remain indexed until done.
    async fn kill(&self, ending: Ending) -> Vec<EpisodeDone> {
        let Some(supervisor) = self.supervisor.upgrade() else {
            return Vec::new();
        };
        let (first, done) = supervisor.request_team_end(self, ending.clone());
        if first {
            let (status, note) = match ending {
                Ending::Killed(note) => (RosterStatus::Failed, Some(note)),
                _ => (RosterStatus::Idle, None),
            };
            if let Err(error) = self
                .record_activity(None, scope::ROOT_HANDLE.into(), status, note)
                .await
            {
                tracing::error!(root = %self.root, %error, "team lead terminal activity failed");
            }
        }
        done
    }

    /// Open `session`'s member row on its parent's log (`running`) as its
    /// turn starts, unless the row already is: one transition per episode
    /// (the first turn after the spawn, or after a revival reopened a row a
    /// report or archive closed), not one per wake. The row folds from the
    /// log, so a restart re-reads it instead of re-emitting. Observability
    /// only: a failed append is logged and never fails the turn.
    async fn mark_member_running(&self, session: SessionId, claim: Option<&ActorClaim>) {
        let Ok(child_projection) = self.engine.read_projection_shared(session).await else {
            return;
        };
        let Some(parent) = child_projection.session.parent else {
            return;
        };
        let Ok(parent_projection) = self.engine.read_projection_shared(parent).await else {
            return;
        };
        let Some(row) = parent_projection
            .session
            .members
            .iter()
            .find(|row| row.child == Some(session))
        else {
            return;
        };
        if row.status == MemberRunStatus::Running {
            return;
        }
        let member = row.member;
        let result = match claim {
            Some(claim) => {
                self.engine
                    .commit_resident_mutation(
                        claim,
                        parent,
                        vec![Event::MemberStatusChanged {
                            session: parent,
                            member,
                            status: MemberRunStatus::Running,
                        }],
                    )
                    .await
            }
            None => {
                self.engine
                    .record_member_status(parent, member, MemberRunStatus::Running)
                    .await
            }
        };
        if let Err(error) = result {
            tracing::warn!("resident member row stayed {:?}: {error}", row.status);
        }
    }

    /// Run exactly one turn for `session`: inject the initial directive (once), the
    /// synthesis directive (main, on quiescence), and every inbox message since the
    /// cursor, then advance the cursor and stream one turn. All of this is coalesced
    /// into a single turn — many queued messages produce one turn, never several.
    ///
    /// Returns `Ok(false)` without running when a main-actor wake has nothing
    /// left to deliver: mail that arrived during the lead's own turn and was
    /// already steered into it needs no follow-up turn.
    async fn run_one_turn(
        &self,
        session: SessionId,
        plan: RunPlan,
        sidecar_tools: Arc<[ResolvedTool]>,
    ) -> Result<bool, CoreError> {
        let RunPlan {
            agent,
            binding,
            agents,
            resources,
            guidance,
            route,
            sidecar_factory: _,
            handle,
            is_main,
            cursor,
            synth,
            initial_prompt,
            claim,
            cancel,
            lease,
        } = plan;
        // Snapshot new inbox mail for this handle (folded before its wake, so it is
        // already visible here). The durable cursor leads the in-memory slot
        // cursor when steer already surfaced mail inside a tool result (or a
        // `wait` returned it) — sync so the wake never re-injects delivered
        // messages as user prompts. The plan's cursor was captured before
        // this sync, so it advances here too.
        let projection = self.engine.read_projection(self.root).await?;
        if !self.owns_slot(session, claim.as_ref()) {
            return Ok(false);
        }
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let mut cursor = cursor;
        if let Some(entry) = projection.team.roster.get(&handle) {
            let durable = usize::try_from(entry.resident_cursor).unwrap_or(usize::MAX);
            cursor = cursor.max(durable);
            let mut st = self.lock();
            if let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            {
                slot.cursor = slot.cursor.max(durable);
            }
        }
        let inbox_len = projection
            .team
            .inboxes
            .get(&handle)
            .map_or(0, |inbox| inbox.len());
        let channel_policy = ChannelPolicy::from_binding(&binding)?;
        let new_mail: Vec<(String, String)> = projection
            .team
            .inboxes
            .get(&handle)
            .map(|inbox| {
                inbox
                    .iter()
                    .skip(cursor)
                    .filter(|message| {
                        if message.from == scope::HARNESS_HANDLE {
                            return true;
                        }
                        let (kind, role) = recipient_channel_role(
                            &projection,
                            &message.to,
                            &handle,
                            &message.from,
                        );
                        resident_delivery_allowed(&channel_policy, agent.name.as_str(), kind, role)
                    })
                    .map(|m| (m.from.clone(), m.body.clone()))
                    .collect()
            })
            .unwrap_or_default();

        if initial_prompt.is_none() && !synth && new_mail.is_empty() {
            let mut st = self.lock();
            if let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            {
                slot.cursor = slot.cursor.max(inbox_len);
            }
            return Ok(false);
        }

        // An empty notification is not a turn and consumes no team budget.
        let exceeded = {
            let mut state = self.lock();
            state.turns_used = state.turns_used.saturating_add(1);
            self.engine.governor().is_some_and(|gov| {
                let limit = gov.limits().per_team_turn_budget;
                limit != 0 && state.turns_used > limit
            })
        };
        if exceeded {
            self.kill(Ending::Killed("per-team turn budget exceeded".into()))
                .await;
            return Err(CoreError::Cancelled);
        }
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
        if !self.owns_slot(session, claim.as_ref()) {
            return Ok(false);
        }
        if !is_main {
            self.mark_member_running(session, claim.as_ref()).await;
        }
        // Harness heartbeat at the turn boundary: liveness is visible the
        // moment work starts, before any engine event lands on the bus.
        self.heartbeat_turn_start(session, claim.as_ref(), &handle)
            .await;
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
        let mut admitted_prompt = initial_prompt;
        for (from, body) in &new_mail {
            if !self.owns_slot(session, claim.as_ref()) {
                return Ok(false);
            }
            let prompt = format!("[mail from {from}] {body}");
            admitted_prompt = Some(match claim.as_ref() {
                Some(claim) => {
                    self.engine
                        .admit_user_prompt_for_actor_with_binding(
                            claim,
                            session,
                            prompt,
                            &binding,
                            agent.name.as_str(),
                        )
                        .await?
                }
                None => {
                    self.engine
                        .admit_user_prompt_with_binding(
                            session,
                            prompt,
                            &binding,
                            agent.name.as_str(),
                        )
                        .await?
                }
            });
        }
        // Advance the cursor so a follow-up turn never re-injects the same mail.
        {
            let mut st = self.lock();
            let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            else {
                return Ok(false);
            };
            slot.cursor = slot.cursor.max(inbox_len);
            slot.initial_prompt = None;
        }
        // Exactly one turn, under the team-wide cancel so a budget kill stops it.
        match (claim.as_ref(), agents, resources) {
            (Some(claim), Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools_for_actor_and_workflow_route(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        TurnRequestContext::new(cancel.clone(), &[], guidance, Some(claim), route)
                            .with_admitted_prompt(admitted_prompt)
                            .with_lease(lease),
                    )
                    .await?;
            }
            (None, Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools_and_workflow_route(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        TurnRequestContext::new(cancel.clone(), &[], guidance, None, route)
                            .with_admitted_prompt(admitted_prompt)
                            .with_lease(lease),
                    )
                    .await?;
            }
            (Some(claim), _, _) => {
                self.engine
                    .run_bound_turn_for_actor_with_workflow_route(
                        session,
                        &agent,
                        binding,
                        TurnRequestContext::new(cancel.clone(), &[], guidance, Some(claim), route)
                            .with_admitted_prompt(admitted_prompt)
                            .with_lease(lease),
                    )
                    .await?;
            }
            (None, _, _) => {
                self.engine
                    .run_bound_turn_with_workflow_route(
                        session,
                        &agent,
                        binding,
                        TurnRequestContext::new(cancel, &[], guidance, None, route)
                            .with_admitted_prompt(admitted_prompt)
                            .with_lease(lease),
                    )
                    .await?;
            }
        }
        // Turn finished: a fresh boundary heartbeat closes the window even
        // when the turn's own engine events never tripped the throttle.
        self.heartbeat_turn_finish(session, claim.as_ref(), &handle)
            .await;
        Ok(true)
    }

    /// Resolve a `MailSent`'s recipient sessions, EXCLUDING the sender's own handle
    /// (self-wake avoidance) and any handle that is not a registered resident.
    async fn recipients(
        &self,
        from: &str,
        to: &MailEndpoint,
    ) -> Vec<(SessionId, Option<ActorClaim>)> {
        let Ok(projection) = self.engine.read_projection(self.root).await else {
            return Vec::new();
        };
        let handles: Vec<String> = match to {
            MailEndpoint::Handle(handle) => {
                let handle = handle.to_string();
                if handle == from {
                    Vec::new() // a self-addressed direct mail never self-wakes
                } else {
                    vec![handle]
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
                    .and_then(|session| {
                        st.residents.get(&session).map(|slot| (session, slot.claim))
                    })
            })
            .collect()
    }

    fn pending_report(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
    ) -> Option<PendingReport> {
        self.lock()
            .residents
            .get(&session)
            .filter(|slot| slot.claim.as_ref() == claim && slot.ending.is_none())
            .and_then(|slot| slot.report.clone())
    }

    fn take_episode(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
    ) -> Option<EpisodeCompletion> {
        let done = {
            let mut state = self.lock();
            let std::collections::hash_map::Entry::Occupied(entry) = state.residents.entry(session)
            else {
                return None;
            };
            if entry.get().claim.as_ref() != claim {
                return None;
            }
            let slot = entry.remove();
            if slot.status == SlotStatus::Busy {
                state.busy = state.busy.saturating_sub(1);
            }
            if state.main_session == Some(session) {
                state.main_session = None;
            }
            self.maybe_fire_quiescence(&mut state);
            slot.done
        };
        self.forget_episode(session, claim);
        Some(done)
    }

    fn complete_episode(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        result: Result<(), CoreError>,
    ) {
        if let Some(done) = self.take_episode(session, claim) {
            done.send_replace(Some(result));
        }
    }

    /// Handle one `MailSent` for this team: charge the message budget, count the
    /// work, and deliver a wake to every resident recipient (never the sender).
    async fn on_mail(&self, from: &str, to: &MailEndpoint) {
        let exceeded = {
            let mut state = self.lock();
            if state.phase.killed() {
                return;
            }
            state.messages_used = state.messages_used.saturating_add(1);
            self.engine.governor().is_some_and(|gov| {
                let limit = gov.limits().per_team_message_budget;
                limit != 0 && state.messages_used > limit
            })
        };
        if exceeded {
            self.kill(Ending::Killed("per-team message budget exceeded".into()))
                .await;
            return;
        }
        {
            let mut st = self.lock();
            if st.phase.killed() {
                return;
            }
            st.work_seq = st.work_seq.saturating_add(1);
        }
        let recipients = self.recipients(from, to).await;
        let Ok(projection) = self.engine.read_projection(self.root).await else {
            return;
        };
        let mut consumed = Vec::new();
        {
            let mut st = self.lock();
            for (session, claim) in recipients {
                if let Some(slot) = st
                    .residents
                    .get_mut(&session)
                    .filter(|slot| slot.claim == claim)
                {
                    let (kind, role) = recipient_channel_role(&projection, to, &slot.handle, from);
                    // Harness mail (leader-failed notice) is a control message:
                    // it bypasses the channel policy.
                    let allowed = from == scope::HARNESS_HANDLE
                        || ChannelPolicy::from_binding(&slot.binding).is_ok_and(|policy| {
                            resident_delivery_allowed(&policy, slot.agent.name.as_str(), kind, role)
                        });
                    if !allowed {
                        let inbox_len = projection
                            .team
                            .inboxes
                            .get(&slot.handle)
                            .map_or(0, |inbox| inbox.len());
                        if !slot.pending && inbox_len == slot.cursor.saturating_add(1) {
                            consumed.push((session, slot.claim, slot.handle.clone(), inbox_len));
                        }
                        continue;
                    }
                    let inbox_len = projection
                        .team
                        .inboxes
                        .get(&slot.handle)
                        .map_or(0, Vec::len);
                    let durable = projection.team.roster.get(&slot.handle).map_or(0, |entry| {
                        usize::try_from(entry.resident_cursor).unwrap_or(usize::MAX)
                    });
                    if inbox_len > slot.cursor.max(durable) {
                        slot.pending = true;
                        slot.notify.notify_one();
                    }
                }
            }
        }
        for (session, claim, handle, through) in consumed {
            if self
                .engine
                .emit_for_actor(
                    claim.as_ref(),
                    self.root,
                    Event::MailConsumed {
                        session: self.root,
                        handle: handle.clone(),
                        through: through as u64,
                    },
                )
                .await
                .is_ok()
                && let Some(slot) = self.lock().residents.get_mut(&session)
                && slot.handle == handle
                && slot.claim == claim
            {
                slot.cursor = slot.cursor.max(through);
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
        let mut consumed = Vec::new();
        {
            let mut st = self.lock();
            if st.phase.killed() {
                return;
            }
            for slot in st.residents.values_mut() {
                let inbox = projection
                    .team
                    .inboxes
                    .get(&slot.handle)
                    .map_or(&[][..], Vec::as_slice);
                if inbox.len() <= slot.cursor {
                    continue;
                }
                let policy = ChannelPolicy::from_binding(&slot.binding).ok();
                let has_deliverable = inbox.iter().skip(slot.cursor).any(|message| {
                    if message.from == scope::HARNESS_HANDLE {
                        return true;
                    }
                    let (kind, role) = recipient_channel_role(
                        &projection,
                        &message.to,
                        &slot.handle,
                        &message.from,
                    );
                    policy.as_ref().is_some_and(|policy| {
                        resident_delivery_allowed(policy, slot.agent.name.as_str(), kind, role)
                    })
                });
                if has_deliverable {
                    slot.pending = true;
                    slot.notify.notify_one();
                } else {
                    consumed.push((slot.claim, slot.handle.clone(), inbox.len()));
                }
            }
        }
        for (claim, handle, through) in consumed {
            if self
                .engine
                .emit_for_actor(
                    claim.as_ref(),
                    self.root,
                    Event::MailConsumed {
                        session: self.root,
                        handle: handle.clone(),
                        through: through as u64,
                    },
                )
                .await
                .is_ok()
                && let Some(slot) = self
                    .lock()
                    .residents
                    .values_mut()
                    .find(|slot| slot.handle == handle && slot.claim == claim)
            {
                slot.cursor = slot.cursor.max(through);
            }
        }
    }
}

fn recipient_channel_role(
    projection: &hya_proto::Projection,
    to: &MailEndpoint,
    recipient: &str,
    sender: &str,
) -> (ChannelTemplateKind, ChannelParticipantRole) {
    if let MailEndpoint::Channel(channel) = to
        && let Some(state) = projection.team.channels.get(channel)
        && state.kind == ChannelKind::Group
    {
        return (
            ChannelTemplateKind::Unit,
            if state.unit.as_deref() == Some(recipient) {
                ChannelParticipantRole::UnitLeader
            } else {
                ChannelParticipantRole::DirectReports
            },
        );
    }
    (
        ChannelTemplateKind::ParentDm,
        if scope::parent_path(sender) == Some(recipient) {
            ChannelParticipantRole::Parent
        } else {
            ChannelParticipantRole::Child
        },
    )
}

fn resident_delivery_allowed(
    policy: &ChannelPolicy,
    agent: &str,
    kind: ChannelTemplateKind,
    role: ChannelParticipantRole,
) -> bool {
    [ChannelCapability::ResidentMail, ChannelCapability::FollowUp]
        .into_iter()
        .all(|capability| match kind {
            ChannelTemplateKind::Unit => policy.allows_unit(capability, agent, role),
            ChannelTemplateKind::ParentDm => policy.allows_parent_dm(capability, agent, role),
        })
}

/// The resident actor loop for one session: park at zero cost, then run turns
/// (with follow-ups for mail that arrived mid-turn) until it owes none, then park
/// again. Exits when the team is killed or the supervisor is dropped.
struct EpisodeInit {
    binding: TurnBinding,
    agent: hya_proto::AgentName,
    parent: SessionId,
    envelopes: Vec<hya_proto::Envelope>,
    ready: oneshot::Sender<Result<(), CoreError>>,
}

async fn initialize_episode(
    team: &TeamActor,
    claim: &ActorClaim,
    cancel: &CancellationToken,
    init: EpisodeInit,
) -> Result<(), CoreError> {
    let EpisodeInit {
        binding,
        agent,
        parent,
        envelopes,
        ready,
    } = init;
    let session = claim.actor_id;
    let created = envelopes.iter().any(|envelope| {
        matches!(envelope.event,
        Event::SessionCreated { session: id, .. } if id == session)
    });
    let initial = envelopes
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            Event::TextDelta {
                session: id,
                message,
                part,
                delta,
            } if *id == session => Some((*message, *part, delta.clone())),
            _ => None,
        });
    let hooks = async {
        if created {
            team.engine.notify_session_start(session).await;
        }
        for hooks in team
            .engine
            .replace_session_hooks(session, &binding, agent.as_str())
        {
            hooks.session_start(SessionLifecycleInput { session }).await;
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {},
        () = hooks => {},
    }
    // Registration is already durable. Even cancellation must publish it before
    // the terminal events; a stale initializer can never publish after revival.
    for envelope in envelopes {
        team.engine.publish_envelope(envelope);
    }
    let initialize = async {
        if let Some((message, part, original)) = initial {
            let text = team
                .engine
                .user_text_after_hooks(&binding, agent.as_str(), session, original.clone())
                .await;
            if text != original {
                team.engine
                    .commit_resident_mutation(
                        claim,
                        session,
                        vec![Event::TextReplace {
                            session,
                            message,
                            part,
                            text,
                        }],
                    )
                    .await?;
            }
        }
        if let Some(hooks) = team.engine.session_hook_dispatcher(parent) {
            hooks
                .agent_spawn(AgentSpawnInput {
                    parent: team.root,
                    child: session,
                })
                .await;
        }
        Ok::<(), CoreError>(())
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        result = initialize => result,
    };
    let result = if cancel.is_cancelled() {
        Err(CoreError::Cancelled)
    } else {
        result
    };
    let _ = ready.send(result.clone());
    result
}

async fn episode_task(
    team: Arc<TeamActor>,
    session: SessionId,
    mut task_claim: Option<ActorClaim>,
    notify: Arc<Notify>,
    cancel: CancellationToken,
    init: Option<EpisodeInit>,
) {
    let mut sidecar_handle: Option<Box<dyn SidecarHandle>> = None;
    let mut sidecar_tools: Arc<[ResolvedTool]> = Arc::from([]);
    let mut sidecar_hooks: Option<Arc<dyn HookDispatcher>> = None;
    if let Some(init) = init {
        let Some(claim) = task_claim.as_ref() else {
            return;
        };
        let result = initialize_episode(&team, claim, &cancel, init).await;
        if let Err(error) = result {
            if cancel.is_cancelled()
                && team
                    .lock()
                    .residents
                    .get(&session)
                    .is_none_or(|slot| slot.ending.is_none())
            {
                team.kill(Ending::Shutdown).await;
            }
            finish_episode(
                &team,
                claim,
                Ending::Failed(format!("resident initialization failed: {error}")),
                &mut sidecar_handle,
                None,
            )
            .await;
            return;
        }
    }
    loop {
        if cancel.is_cancelled() && !team.lock().phase.killed() {
            let requested = team
                .lock()
                .residents
                .get(&session)
                .is_some_and(|slot| slot.ending.is_some());
            if !requested {
                team.kill(Ending::Shutdown).await;
            }
        }
        if !team.owns_slot(session, task_claim.as_ref()) {
            if let Some(handle) = sidecar_handle.as_mut() {
                let _ = handle.terminate().await;
            }
            return;
        }
        let ending = team
            .lock()
            .residents
            .get(&session)
            .and_then(|slot| slot.ending.clone());
        if let Some(ending) = ending {
            if let Some(claim) = task_claim.as_ref() {
                finish_episode(&team, claim, ending, &mut sidecar_handle, None).await;
            } else {
                team.complete_episode(session, None, Ok(()));
            }
            return;
        }
        if let (Some(claim), Some(pending)) = (
            task_claim.as_ref(),
            team.pending_report(session, task_claim.as_ref()),
        ) {
            let ending = Ending::Reported(pending);
            match commit_episode_ending(&team, claim, &ending, None).await {
                Err(CoreError::Cancelled) => {
                    if team
                        .lock()
                        .residents
                        .get(&session)
                        .is_none_or(|slot| slot.ending.is_none())
                    {
                        team.kill(Ending::Shutdown).await;
                    }
                    continue;
                }
                Ok((hya_store::ResidentArchiveOutcome::Refused, _)) => {
                    let mut state = team.lock();
                    if let Some(slot) = state
                        .residents
                        .get_mut(&session)
                        .filter(|slot| slot.claim == task_claim)
                    {
                        slot.report = None;
                    }
                }
                result => {
                    finish_episode(
                        &team,
                        claim,
                        ending,
                        &mut sidecar_handle,
                        Some(result.map(|(_, finalization)| finalization)),
                    )
                    .await;
                    return;
                }
            }
        }
        if let Some(loss_token) = sidecar_handle
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
                _ = cancel.cancelled() => continue,
                _ = notify.notified() => {}
            }
        } else {
            tokio::select! {
                _ = cancel.cancelled() => continue,
                _ = notify.notified() => {}
            }
        }
        loop {
            match team.next_action(session, task_claim.as_ref()).await {
                Action::Run(plan) => {
                    let handle = plan.handle.clone();
                    let claim = plan.claim;
                    let is_main = plan.is_main;
                    if sidecar_handle
                        .as_ref()
                        .is_some_and(|cached| !cached.is_healthy())
                        && let Some(mut stale_handle) = sidecar_handle.take()
                    {
                        sidecar_tools = Arc::from([]);
                        sidecar_hooks = None;
                        if let Err(err) = stale_handle.terminate().await {
                            if !team.owns_slot(session, task_claim.as_ref()) {
                                continue;
                            }
                            let reason = format!("turn error: {err}");
                            let finalized = archive_stopped_agent(
                                &team.engine,
                                team.root,
                                &handle,
                                session,
                                claim.as_ref(),
                                ArchiveReason::Killed,
                                MemberRunStatus::Failed,
                                FinishCause::Other,
                                &reason,
                            )
                            .await;
                            if finalized.is_ok() {
                                team.remove_slot(session, task_claim.as_ref());
                                return;
                            }
                            team.complete_episode(session, task_claim.as_ref(), finalized);
                            return;
                        }
                    }
                    if sidecar_handle.is_none()
                        && let Some(factory) = plan.sidecar_factory.clone()
                    {
                        let cancel = plan.cancel.clone();
                        let started = tokio::select! {
                            biased;
                            _ = cancel.cancelled() => Err(CoreError::Cancelled),
                            result = factory.start(SidecarStart::resident()) => result,
                        };
                        let activation = match started {
                            Ok(mut handle) => {
                                let result = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => Err(CoreError::Cancelled),
                                    result = handle.ready() => result,
                                };
                                sidecar_handle = Some(handle);
                                result
                            }
                            Err(error) => Err(error),
                        };
                        if let Err(error) = activation {
                            drop(plan);
                            let Some(claim) = claim.as_ref() else {
                                return;
                            };
                            finish_episode(
                                &team,
                                claim,
                                Ending::Failed(format!("sidecar start failed: {error}")),
                                &mut sidecar_handle,
                                None,
                            )
                            .await;
                            return;
                        }
                        if let Some(handle) = sidecar_handle.as_ref() {
                            sidecar_tools = handle.tool_bindings();
                            sidecar_hooks = handle
                                .hook_dispatcher()
                                .map(crate::bundle_hooks::restricted_sidecar_hooks);
                        }
                    }
                    let loss_token = sidecar_handle
                        .as_ref()
                        .and_then(|handle| handle.loss_token());
                    let turn_cancel = plan.cancel.clone();
                    let turn = team.run_one_turn(session, *plan, Arc::clone(&sidecar_tools));
                    let hooks = sidecar_hooks.clone();
                    let mut run = Box::pin(async move {
                        match hooks {
                            Some(hooks) => scope_activation_hooks(session, hooks, turn).await,
                            None => turn.await,
                        }
                    });
                    let mut completion = Box::pin(async {
                        if let Some(loss_token) = loss_token {
                            tokio::select! {
                                biased;
                                _ = loss_token.cancelled() => {
                                    turn_cancel.cancel();
                                    (run.as_mut().await.and(Err(CoreError::Cancelled)), true)
                                }
                                result = &mut run => (result, false),
                            }
                        } else {
                            (run.as_mut().await, false)
                        }
                    });
                    let (turn_result, sidecar_lost) = tokio::select! {
                        result = &mut completion => result,
                        _ = cancel.cancelled() => {
                            if team.lock().residents.get(&session).is_none_or(|slot| slot.ending.is_none()) {
                                team.kill(Ending::Shutdown).await;
                            }
                            let ending = team.lock().residents.get(&session).and_then(|slot| slot.ending.clone());
                            if let Some(ending) = ending {
                                match tokio::time::timeout(DRAIN_DEADLINE, &mut completion).await {
                                    Ok(result) => result,
                                    Err(_) => {
                                        let close = async {
                                            if let Some(claim) = claim.as_ref() {
                                                team.engine.close_resident_turn(claim, ending.cause()).await?;
                                            }
                                            Ok::<(), CoreError>(())
                                        };
                                        // Keep polling the turn while its message lock is acquired;
                                        // it may itself be finishing a fenced append under that lock.
                                        tokio::select! {
                                            result = &mut completion => result,
                                            result = close => {
                                                if let Err(error) = result {
                                                    tracing::error!(%session, ?claim, %error, "episode straggler close failed");
                                                }
                                                (Err(CoreError::Cancelled), false)
                                            }
                                        }
                                    }
                                }
                            } else {
                                completion.as_mut().await
                            }
                        }
                    };
                    drop(completion);
                    drop(run);
                    if !team.owns_slot(session, task_claim.as_ref()) {
                        continue;
                    }
                    let running_loss = claim.is_some()
                        && matches!(&turn_result, Err(CoreError::Cancelled))
                        && (sidecar_lost
                            || sidecar_handle
                                .as_ref()
                                .is_some_and(|cached| !cached.is_healthy()));
                    if running_loss {
                        let Some(old_claim) = claim else {
                            team.finish_run(session, task_claim.as_ref());
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
                                if !team.owns_slot(session, task_claim.as_ref()) {
                                    continue;
                                }
                                if let Some(mut stale_handle) = sidecar_handle.take() {
                                    let _ = stale_handle.terminate().await;
                                }
                                let reason = format!("resident recovery failed: {error}");
                                match archive_stopped_agent(
                                    &team.engine,
                                    team.root,
                                    &handle,
                                    old_claim.actor_id,
                                    Some(&old_claim),
                                    ArchiveReason::Killed,
                                    MemberRunStatus::Failed,
                                    FinishCause::Other,
                                    &reason,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        team.remove_slot(session, task_claim.as_ref());
                                        return;
                                    }
                                    Err(error) => {
                                        team.complete_episode(
                                            session,
                                            task_claim.as_ref(),
                                            Err(error),
                                        );
                                        return;
                                    }
                                }
                            }
                        };
                        if !team.owns_slot(session, task_claim.as_ref()) {
                            continue;
                        }
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
                        if !team.owns_slot(session, task_claim.as_ref()) {
                            continue;
                        }
                        let report = match team
                            .engine
                            .recover_resident_actor(&recovered, team.root, &handle)
                            .await
                        {
                            Ok(report) => report,
                            Err(error) => {
                                if !team.owns_slot(session, task_claim.as_ref()) {
                                    continue;
                                }
                                let claim_replaced = team
                                    .replace_recovered_claim(
                                        session,
                                        old_claim,
                                        recovered.claim,
                                        ResidentRecovery::Idle,
                                    )
                                    .is_ok();
                                if !claim_replaced {
                                    continue;
                                }
                                task_claim = Some(recovered.claim);
                                let reason = format!("resident recovery failed: {error}");
                                match archive_stopped_agent(
                                    &team.engine,
                                    team.root,
                                    &handle,
                                    recovered.claim.actor_id,
                                    Some(&recovered.claim),
                                    ArchiveReason::Killed,
                                    MemberRunStatus::Failed,
                                    FinishCause::Other,
                                    &reason,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        team.remove_slot(session, task_claim.as_ref());
                                        return;
                                    }
                                    Err(error) => {
                                        team.complete_episode(
                                            session,
                                            task_claim.as_ref(),
                                            Err(error),
                                        );
                                        return;
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
                            team.finish_run(session, task_claim.as_ref());
                            return;
                        }
                        task_claim = Some(recovered.claim);
                        if termination_error.is_some() {
                            match archive_stopped_agent(
                                &team.engine,
                                team.root,
                                &handle,
                                recovered.claim.actor_id,
                                Some(&recovered.claim),
                                ArchiveReason::Killed,
                                MemberRunStatus::Cancelled,
                                FinishCause::UserCancel,
                                "resident stopped",
                            )
                            .await
                            {
                                Ok(()) => {
                                    // A missing slot means local cleanup already completed; the
                                    // durable finalizer has already made the stop terminal.
                                    team.remove_slot(session, task_claim.as_ref());
                                    return;
                                }
                                Err(error) => {
                                    team.complete_episode(session, task_claim.as_ref(), Err(error));
                                    return;
                                }
                            }
                        }
                        team.finish_run(session, task_claim.as_ref());
                        continue;
                    }
                    // Flip the slot idle BEFORE recording the roster activity:
                    // observers gate on "roster says idle" (the report gate),
                    // so that observation must imply the slot is idle too.
                    team.finish_run(session, task_claim.as_ref());
                    match turn_result {
                        Ok(false) => {}
                        Ok(true) => {
                            let _ = team
                                .record_activity(claim.as_ref(), handle, RosterStatus::Idle, None)
                                .await;
                        }
                        Err(CoreError::Cancelled) => {
                            // Cancellation alone is not terminal. The owning
                            // stop/kill/archive path commits terminal effects.
                        }
                        Err(err) if is_main => {
                            // The lead is never archived: a failed lead turn
                            // ended with `finish: error`, its team was told
                            // to wrap up, and the session stays live and
                            // resumable. The slot parks until the user
                            // resumes the lead.
                            let _ = team
                                .record_activity(
                                    claim.as_ref(),
                                    handle,
                                    RosterStatus::Idle,
                                    Some(format!("turn failed: {err}")),
                                )
                                .await;
                        }
                        Err(err) => {
                            let Some(claim) = claim.as_ref() else {
                                return;
                            };
                            finish_episode(
                                &team,
                                claim,
                                Ending::Failed(format!("turn error: {err}")),
                                &mut sidecar_handle,
                                None,
                            )
                            .await;
                            return;
                        }
                    }
                    // An accepted report executes at rest before any
                    // follow-up turn: mail the turn already steered in owes
                    // no new episode, and genuinely unread mail fails the
                    // at-rest gate re-check, which re-arms the wake.
                    if team.pending_report(session, task_claim.as_ref()).is_some()
                        || team
                            .lock()
                            .residents
                            .get(&session)
                            .is_some_and(|slot| slot.ending.is_some())
                    {
                        break;
                    }
                }
                Action::End(ending) => {
                    if let Some(claim) = task_claim.as_ref() {
                        finish_episode(&team, claim, ending, &mut sidecar_handle, None).await;
                    } else {
                        team.complete_episode(session, None, Ok(()));
                    }
                    return;
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
    /// Includes episodes on teams already removed by a kill.
    episodes: Mutex<HashMap<SessionId, Arc<TeamActor>>>,
    /// Claim commit + install and orphan Takeover share this boundary across
    /// team generations. Kills never wait for it or for registration hooks.
    claim_writes: tokio::sync::Mutex<()>,
    self_ref: Weak<ResidentSupervisor>,
    listener_stop: CancellationToken,
    listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// `wait` stall notices already given: (waiting session, member session)
    /// → the member's last assistant message when it was reported stopped.
    /// In-memory only: after a restart a stopped member is reported once more.
    wait_stalls: Mutex<HashMap<(SessionId, SessionId), Option<hya_proto::MessageId>>>,
}

type EpisodeFinalization = (Vec<hya_proto::Envelope>, Vec<hya_store::AdmissionRecord>);

async fn commit_episode_ending(
    team: &TeamActor,
    claim: &ActorClaim,
    ending: &Ending,
    sidecar_error: Option<String>,
) -> Result<(hya_store::ResidentArchiveOutcome, EpisodeFinalization), CoreError> {
    let (handle, child, cancel) = {
        let state = team.lock();
        let slot = state
            .residents
            .get(&claim.actor_id)
            .filter(|slot| slot.claim == Some(*claim))
            .ok_or_else(|| CoreError::Invalid("episode ownership changed".into()))?;
        (slot.handle.clone(), claim.actor_id, slot.cancel.clone())
    };
    let projection = team.engine.read_projection(team.root).await?;
    let expected_work = projection
        .team
        .roster
        .get(&handle)
        .map(|entry| (entry.resident_cursor, entry.resident_work));
    let handoff = match ending {
        Ending::Reported(_) => {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(CoreError::Cancelled),
                handoff = team.engine.terminal_handoff_claimed(child) => handoff,
            }
        }
        _ => team.engine.degraded_terminal_handoff(child).await,
    };
    let (reason, status, report, mut note, check_gate) = match ending {
        Ending::Reported(pending) => (
            ArchiveReason::Reported,
            match pending.outcome {
                ReportOutcome::Done => MemberRunStatus::Done,
                ReportOutcome::Failed => MemberRunStatus::Failed,
            },
            Some((pending.outcome, pending.report.clone())),
            pending.report.clone(),
            true,
        ),
        Ending::Failed(note) => (
            ArchiveReason::Reported,
            MemberRunStatus::Failed,
            Some((ReportOutcome::Failed, note.clone())),
            note.clone(),
            false,
        ),
        ending => {
            let (reason, status, note) = ending.forced_fields();
            (reason, status, None, note, false)
        }
    };
    if let Some(error) = sidecar_error {
        note.push_str(&format!("; sidecar did not stop: {error}"));
    }
    let (outcome, envelopes, admissions) = team
        .engine
        .store()
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Claim(claim),
            team.root,
            &handle,
            hya_store::ResidentArchive {
                actor: child,
                generation: handoff.generation,
                doc: handoff.doc,
                degraded: handoff.degraded,
                reason,
                note,
                member_status: status,
                report,
                check_gate,
                expected_work,
            },
            ending.cause(),
        )
        .await?;
    if outcome == hya_store::ResidentArchiveOutcome::Refused {
        team.engine
            .publish_resident_finalization((envelopes, admissions));
        Ok((outcome, (Vec::new(), Vec::new())))
    } else {
        Ok((outcome, (envelopes, admissions)))
    }
}

/// Finish once, even on a failed store write. The durable active claim then
/// belongs to an orphan, which a later archive reconciles with Takeover.
async fn finish_episode(
    team: &TeamActor,
    claim: &ActorClaim,
    mut ending: Ending,
    sidecar: &mut Option<Box<dyn SidecarHandle>>,
    committed: Option<Result<EpisodeFinalization, CoreError>>,
) {
    if committed.is_none() {
        let mut state = team.lock();
        if let Some(slot) = state
            .residents
            .get_mut(&claim.actor_id)
            .filter(|slot| slot.claim == Some(*claim))
        {
            ending = slot.ending.get_or_insert(ending).clone();
        }
    }
    let sidecar_error = if let Some(mut handle) = sidecar.take() {
        match tokio::time::timeout(Duration::from_secs(5), handle.terminate()).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("deadline exceeded".into()),
        }
    } else {
        None
    };
    if committed.is_some()
        && let Some(error) = &sidecar_error
    {
        tracing::error!(session = %claim.actor_id, %error, "reported episode sidecar did not stop");
    }
    let result = match committed {
        Some(result) => result,
        None => commit_episode_ending(team, claim, &ending, sidecar_error)
            .await
            .map(|(_, finalization)| finalization),
    };
    if let Err(error) = &result {
        tracing::error!(session = %claim.actor_id, ?claim, %error, "episode terminal transition failed; leaving orphan");
    }
    // No archive envelope is observable while this episode is installed. Hold
    // the claim-write boundary until publication and done, so revival cannot
    // publish a new registration ahead of the old archive.
    let supervisor = team.supervisor.upgrade();
    let _write = match &supervisor {
        Some(supervisor) => Some(supervisor.claim_writes.lock().await),
        None => None,
    };
    if let Some(done) = team.take_episode(claim.actor_id, Some(claim)) {
        let result =
            result.map(|finalization| team.engine.publish_resident_finalization(finalization));
        done.send_replace(Some(result));
    }
}

/// Explicit stop uses the same terminal transaction without parent report mail.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn archive_stopped_agent(
    engine: &SessionEngine,
    root: SessionId,
    canonical: &str,
    child: SessionId,
    claim: Option<&ActorClaim>,
    reason: ArchiveReason,
    member_status: MemberRunStatus,
    cause: FinishCause,
    note: &str,
) -> Result<(), CoreError> {
    if child == root || canonical == scope::ROOT_HANDLE {
        return Err(CoreError::Invalid(
            "the team lead is never archived".to_string(),
        ));
    }
    let handoff = engine.degraded_terminal_handoff(child).await;

    let (_, envelopes, admissions) = engine
        .store()
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Claim(claim.ok_or_else(|| {
                CoreError::Invalid("sidecar recovery requires its episode claim".into())
            })?),
            root,
            canonical,
            hya_store::ResidentArchive {
                actor: child,
                generation: handoff.generation,
                doc: handoff.doc,
                degraded: handoff.degraded,
                reason,
                note: note.into(),
                member_status,
                report: None,
                check_gate: false,
                expected_work: None,
            },
            cause,
        )
        .await
        .inspect_err(|error| {
            tracing::error!(session = %child, %error, "resident terminal transition failed");
        })?;
    engine.publish_resident_finalization((envelopes, admissions));
    Ok(())
}

/// One resolved `archive`/`wait` target inside a team.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MemberTarget {
    /// Canonical handle.
    pub handle: String,
    /// The member's session.
    pub session: SessionId,
    /// On the live roster (`false`: archived).
    pub live: bool,
}

/// Resolve a model-supplied subagent reference — canonical handle, a leaf
/// relative to the caller (`hya-worker-exusiai`), a unique leaf, or a session id
/// (`hysec_…`) — against the team's live roster and archive. The lead
/// resolves too (`main` or the root session id); callers decide what that
/// means. The error lists the caller's live subagents.
pub(crate) fn resolve_member_target(
    projection: &hya_proto::Projection,
    root: SessionId,
    caller: &str,
    raw: &str,
) -> Result<MemberTarget, CoreError> {
    let token = raw.trim().trim_start_matches('@');
    let live = |handle: &str| {
        projection
            .team
            .roster
            .get(handle)
            .map(|entry| MemberTarget {
                handle: handle.to_string(),
                session: entry.session,
                live: true,
            })
    };
    let archived = |handle: &str| {
        projection
            .team
            .archived
            .get(handle)
            .map(|entry| MemberTarget {
                handle: handle.to_string(),
                session: entry.session,
                live: false,
            })
    };
    let found = if token.is_empty() {
        None
    } else if let Ok(session) = token.parse::<SessionId>() {
        if session == root {
            Some(MemberTarget {
                handle: scope::ROOT_HANDLE.to_string(),
                session: root,
                live: true,
            })
        } else {
            projection
                .team
                .roster
                .values()
                .find(|entry| entry.session == session)
                .map(|entry| MemberTarget {
                    handle: entry.handle.clone(),
                    session,
                    live: true,
                })
                .or_else(|| {
                    projection
                        .team
                        .archived
                        .values()
                        .find(|entry| entry.session == session)
                        .map(|entry| MemberTarget {
                            handle: entry.handle.clone(),
                            session,
                            live: false,
                        })
                })
        }
    } else if token == scope::ROOT_HANDLE {
        Some(MemberTarget {
            handle: scope::ROOT_HANDLE.to_string(),
            session: root,
            live: true,
        })
    } else {
        let relative = scope::join_path(caller, token);
        live(token)
            .or_else(|| archived(token))
            .or_else(|| live(&relative))
            .or_else(|| archived(&relative))
            .or_else(|| {
                let mut leaves = projection
                    .team
                    .roster
                    .keys()
                    .filter(|key| scope::leaf(key) == token);
                match (leaves.next(), leaves.next()) {
                    (Some(only), None) => live(only),
                    _ => None,
                }
            })
            .or_else(|| {
                let mut leaves = projection
                    .team
                    .archived
                    .keys()
                    .filter(|key| scope::leaf(key) == token);
                match (leaves.next(), leaves.next()) {
                    (Some(only), None) => archived(only),
                    _ => None,
                }
            })
    };
    found.ok_or_else(|| {
        let prefix = format!("{caller}{}", scope::PATH_SEPARATOR);
        let mine: Vec<&str> = projection
            .team
            .roster
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .map(String::as_str)
            .collect();
        let listed = if mine.is_empty() {
            "none".to_string()
        } else {
            mine.join(", ")
        };
        CoreError::Invalid(format!(
            "no subagent `{token}` on your team; your live subagents: {listed}. \
             Name one by the handle or session id the `task` result returned"
        ))
    })
}

/// Projection-level report gate (ADR-0015): live roster row, inbox drained,
/// no live children. Pure read; shared by the tool pre-check, the direct
/// archive path, and the at-rest pending-report execution.
async fn report_gate_projection(
    engine: &Arc<SessionEngine>,
    root: SessionId,
    canonical: &str,
) -> Result<hya_proto::Projection, CoreError> {
    let projection = engine.read_projection(root).await?;
    let entry = projection.team.roster.get(canonical).ok_or_else(|| {
        CoreError::Invalid(format!("resident `{canonical}` is not on the roster"))
    })?;
    let inbox_len = projection
        .team
        .inboxes
        .get(canonical)
        .map_or(0, |inbox| inbox.len() as u64);
    // Mail the current resident wake injected into this turn was read there.
    let read_through = entry.resident_work.map_or(entry.resident_cursor, |work| {
        entry.resident_cursor.max(work.inbox_through)
    });
    if read_through < inbox_len {
        return Err(CoreError::Invalid(unread_mail_rejection(
            &projection,
            canonical,
            read_through,
        )));
    }
    let live_children = projection
        .team
        .roster
        .keys()
        .any(|path| scope::parent_path(path) == Some(canonical));
    if live_children {
        return Err(CoreError::Invalid(format!(
            "report rejected: `{canonical}` still has live children; archive them first"
        )));
    }
    Ok(projection)
}

/// Actionable unread-mail gate error: names each channel holding unread mail
/// and the exact call that reads it, then the reply and `wait` paths. A real
/// run's subagent guessed `#main/scout-1`, `#mail`, `#inbox`… because the old
/// error named no channel.
fn unread_mail_rejection(
    projection: &hya_proto::Projection,
    canonical: &str,
    read_through: u64,
) -> String {
    let unread = projection
        .team
        .inboxes
        .get(canonical)
        .map(|inbox| {
            inbox
                .iter()
                .skip(usize::try_from(read_through).unwrap_or(usize::MAX))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut per_channel: BTreeMap<String, usize> = BTreeMap::new();
    let mut notices = 0usize;
    for message in &unread {
        let channel = match &message.to {
            MailEndpoint::Channel(channel) => Some(channel.clone()),
            // Handle mail travels over the DM pair shared with its sender.
            MailEndpoint::Handle(_) => projection
                .team
                .channels
                .iter()
                .find(|(_, channel)| {
                    channel.kind == ChannelKind::Dm
                        && channel.members.iter().any(|member| member == canonical)
                        && channel.members.iter().any(|member| member == &message.from)
                })
                .map(|(id, _)| id.clone()),
        };
        match channel {
            Some(channel) => *per_channel.entry(channel).or_default() += 1,
            None => notices += 1,
        }
    }
    let mut holders: Vec<String> = per_channel
        .iter()
        .map(|(channel, count)| format!("#{channel} ({count})"))
        .collect();
    if notices > 0 {
        holders.push(format!("{notices} harness notice(s)"));
    }
    let reads: Vec<String> = per_channel
        .iter()
        .map(|(channel, count)| {
            if *count > 1 {
                format!("`read channel://{channel}?last={count}`")
            } else {
                format!("`read channel://{channel}`")
            }
        })
        .collect();
    let read_hint = if reads.is_empty() {
        "Call `list_channel`: pending notices arrive with the next tool result as `[NEW MAIL]`"
            .to_string()
    } else {
        format!(
            "Read it with {} (`list_channel` lists every channel with unread counts)",
            reads.join(" and ")
        )
    };
    format!(
        "report rejected: `{canonical}` has {} unread mail message(s) on {}; answer them first. \
         {read_hint}, reply with `send` if the sender needs an answer, then call `report` \
         again — or call `wait` to block until more mail arrives.",
        unread.len(),
        holders.join(", ")
    )
}

/// Single-active-turn delivery: when any turn on a team slot's session ends,
/// the owning team re-wakes that slot if work was deferred behind the turn and
/// re-checks quiescence.
impl TurnBoundaryObserver for ResidentSupervisor {
    fn mail_mark(&self, session: SessionId) -> Option<u64> {
        self.team_with_resident(session)
            .map(|team| team.lock().work_seq)
    }

    fn mail_consumed(&self, session: SessionId, mark: u64) {
        if let Some(team) = self.team_with_resident(session) {
            team.on_mail_consumed(session, mark);
        }
    }

    fn turn_released(&self, session: SessionId) {
        if let Some(team) = self.team_with_resident(session) {
            team.on_turn_released(session);
        }
    }

    fn turn_started(&self, session: SessionId) {
        if let Some(team) = self.teams().get(&session).cloned() {
            team.on_lead_turn_started(session);
        }
    }

    fn turn_failed(&self, session: SessionId) {
        if let Some(team) = self.teams().get(&session).cloned() {
            team.on_lead_turn_failed(session);
        }
    }
}

/// A downward mail to an archived direct child revives it (ADR-0015): the
/// supervisor is the engine's [`ArchiveReviver`].
#[async_trait::async_trait]
impl ArchiveReviver for ResidentSupervisor {
    fn has_episode(&self, session: SessionId) -> bool {
        self.team_with_resident(session).is_some_and(|team| {
            team.lock()
                .residents
                .get(&session)
                .is_some_and(|slot| slot.claim.is_some())
        })
    }

    async fn reconcile_orphan(
        &self,
        root: SessionId,
        handle: &str,
        archive: hya_store::ResidentArchive,
        cause: FinishCause,
    ) -> Result<hya_store::ResidentArchiveOutcome, CoreError> {
        let _write = self.claim_writes.lock().await;
        if self.team_with_resident(archive.actor).is_some() {
            return Err(CoreError::Invalid(
                "a live episode owns this archive; request its ending".into(),
            ));
        }
        let (outcome, envelopes, admissions) = self
            .engine
            .store()
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                root,
                handle,
                archive,
                cause,
            )
            .await?;
        self.engine
            .publish_resident_finalization((envelopes, admissions));
        Ok(outcome)
    }

    async fn revive(
        &self,
        root: SessionId,
        parent: &str,
        child_handle: &str,
        body: String,
    ) -> Result<(), CoreError> {
        self.revive_child(root, parent, child_handle, body).await
    }

    async fn archive_member(
        &self,
        caller: SessionId,
        target: &str,
        reason: &str,
    ) -> Result<ArchiveReceipt, CoreError> {
        ResidentSupervisor::archive_member(self, caller, target, reason).await
    }

    async fn archive_member_for_user(
        &self,
        root: SessionId,
        target: &str,
    ) -> Result<ArchiveReceipt, CoreError> {
        self.archive_member_noted(root, target, "archived by the user".to_string())
            .await
    }

    fn stop_for_user(&self, session: SessionId) -> Vec<SessionId> {
        self.team_of(session)
            .map(|team| team.stop_for_user(session))
            .unwrap_or_default()
    }

    fn resume_for_user(&self, session: SessionId) {
        if let Some(team) = self.team_of(session) {
            team.resume_for_user(session);
        }
    }

    fn stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        let team = self.teams().get(&root).cloned();
        team.is_some_and(|team| team.handle_stopped_by_user(handle))
    }

    fn session_stopped_by_user(&self, session: SessionId) -> bool {
        self.team_of(session).is_some_and(|team| {
            team.lock()
                .residents
                .get(&session)
                .is_some_and(|slot| slot.user_stopped)
        })
    }

    async fn teardown_root(&self, root: SessionId) -> Result<(), CoreError> {
        let team = self.teams().get(&root).cloned();
        let mut done = match team {
            Some(team) => team.kill(Ending::Teardown).await,
            None => Vec::new(),
        };
        // Wait for any claim commit already in flight to install into its
        // killed team, then request all generations, including old killed teams.
        {
            let _write = self.claim_writes.lock().await;
            let teams = self.episode_teams(root);
            for team in teams {
                done.extend(team.kill(Ending::Teardown).await);
            }
        }
        let results = futures::future::join_all(done.into_iter().map(await_episode)).await;
        // A failed ending has become an orphan. Reconcile it under the same
        // claim-write boundary; never take a member's turn lease from outside.
        self.reconcile_root(root, Ending::Teardown).await?;
        for result in results {
            if let Err(error) = result {
                tracing::error!(%root, %error, "root teardown reconciled a failed ending");
            }
        }
        Ok(())
    }
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
        let supervisor = Arc::new_cyclic(|self_ref| Self {
            engine,
            owner_run_id,
            teams: Mutex::new(HashMap::new()),
            episodes: Mutex::new(HashMap::new()),
            claim_writes: tokio::sync::Mutex::new(()),
            self_ref: self_ref.clone(),
            listener_stop: CancellationToken::new(),
            listener: Mutex::new(None),
            wait_stalls: Mutex::new(HashMap::new()),
        });
        // The supervisor is the engine's revive seam (ADR-0015): wire it before
        // the listener task starts so no mail send can race the installation.
        supervisor
            .engine
            .set_reviver(supervisor.clone() as Arc<dyn ArchiveReviver>);
        // Turn boundaries deliver wakes deferred by the single-active-turn
        // invariant (mail/quiescence owed to a session whose turn was running).
        let observer: Arc<dyn TurnBoundaryObserver> = supervisor.clone();
        supervisor
            .engine
            .set_turn_observer(Arc::downgrade(&observer));
        let listener = supervisor.clone();
        *supervisor
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(tokio::spawn(async move { listener.run_bus(rx).await }));
        supervisor
    }

    fn teams(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Arc<TeamActor>>> {
        match self.teams.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The single, zero-cost wake loop: park on the bus, route each `MailSent` to
    /// its team. On a broadcast lag, re-arm every team's residents so no wake
    /// is lost (the cursor makes re-arming safe — a spurious wake with no new mail
    /// simply idles again).
    ///
    /// Every non-heartbeat envelope also feeds harness liveness (ADR-0002): an
    /// event whose owning session is a registered actor slot proves that slot
    /// is making real progress, and a busy slot emits a throttled
    /// `AgentHeartbeat` for the parent to observe. Heartbeat envelopes
    /// themselves are excluded — liveness derives from agent work only, never
    /// from the harness's own observations (which would also feed a busy main
    /// actor its children's heartbeats as activity).
    async fn run_bus(
        self: Arc<Self>,
        mut rx: tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
    ) {
        loop {
            let received = tokio::select! {
                biased;
                () = self.listener_stop.cancelled() => break,
                received = rx.recv() => received,
            };
            match received {
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
                    if !matches!(envelope.event, Event::AgentHeartbeat { .. })
                        && let Some(actor) = envelope.event.session()
                        && let Some(team) = self.team_with_resident(actor)
                    {
                        team.note_activity(actor).await;
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

    /// The team tracking `session` as a resident slot, if any. Scans tracked
    /// teams, taking each team lock only long enough for a key lookup; the
    /// supervisor lock is held throughout, matching the supervisor→team lock
    /// order every other path obeys.
    fn team_with_resident(&self, session: SessionId) -> Option<Arc<TeamActor>> {
        self.episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .cloned()
            .or_else(|| self.teams().get(&session).cloned())
    }

    /// Resolve before taking the map lock; install main before exposing the new
    /// team. A killed team's old context and counters are never copied forward.
    async fn team_for(&self, root: SessionId) -> Result<Arc<TeamActor>, CoreError> {
        self.team_for_with_main(root, None).await
    }

    async fn team_for_with_main(
        &self,
        root: SessionId,
        main: Option<MainActivation>,
    ) -> Result<Arc<TeamActor>, CoreError> {
        if let Some(team) = self.teams().get(&root).cloned() {
            return Ok(team);
        }
        let main = match main {
            Some(main) => main,
            None => self.resolve_main_activation(root).await?,
        };
        let mut teams = self.teams();
        Ok(teams
            .entry(root)
            .or_insert_with(|| {
                let team = Arc::new(TeamActor::new(
                    root,
                    self.engine.clone(),
                    self.self_ref.clone(),
                ));
                team.install_main(main);
                team
            })
            .clone())
    }

    fn request_team_end(&self, team: &TeamActor, ending: Ending) -> (bool, Vec<EpisodeDone>) {
        let mut teams = self.teams();
        if teams
            .get(&team.root)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), team))
        {
            teams.remove(&team.root);
        }
        let mut state = team.lock();
        let first = !state.phase.killed();
        if first {
            state.phase = TeamPhase::Killed(ending.clone());
        }
        let TeamPhase::Killed(ending) = state.phase.clone() else {
            unreachable!()
        };
        if matches!(ending, Ending::Teardown | Ending::Shutdown) {
            self.engine.cancel_turn(team.root, ending.cause());
        }
        let done = state
            .residents
            .iter_mut()
            .map(|(session, slot)| slot.request_end(&self.engine, *session, ending.clone()))
            .collect();
        team.cancel.cancel();
        (first, done)
    }

    fn retire_team(&self, team: &TeamActor) {
        let mut teams = self.teams();
        if !teams
            .get(&team.root)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), team))
        {
            return;
        }
        let mut state = team.lock();
        if state.phase.killed()
            || state.busy != 0
            || self.engine.turn_active(team.root)
            || state
                .residents
                .values()
                .any(|slot| !slot.is_main || slot.has_work() || slot.report.is_some())
        {
            return;
        }
        teams.remove(&team.root);
        for (session, slot) in &mut state.residents {
            slot.request_end(&self.engine, *session, Ending::Shutdown);
        }
        team.cancel.cancel();
    }

    fn episode_teams(&self, root: SessionId) -> Vec<Arc<TeamActor>> {
        let mut teams = Vec::new();
        for team in self
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            if team.root == root && !teams.iter().any(|current| Arc::ptr_eq(current, team)) {
                teams.push(team.clone());
            }
        }
        teams
    }

    async fn reconcile_root(&self, root: SessionId, ending: Ending) -> Result<(), CoreError> {
        let projection = self.engine.read_projection(root).await?;
        let mut members = projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| path.as_str() != scope::ROOT_HANDLE)
            .collect::<Vec<_>>();
        members.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        for (handle, member) in members {
            self.end_orphan(root, handle, member.session, &ending)
                .await?;
        }
        Ok(())
    }

    async fn end_orphan(
        &self,
        root: SessionId,
        handle: &str,
        actor: SessionId,
        ending: &Ending,
    ) -> Result<(), CoreError> {
        let _write = self.claim_writes.lock().await;
        if self.team_with_resident(actor).is_some() {
            return Err(CoreError::Invalid(
                "a live episode owns this archive; request its ending".into(),
            ));
        }
        let handoff = self.engine.degraded_terminal_handoff(actor).await;
        let (reason, status, note) = ending.forced_fields();
        let (_, envelopes, admissions) = self
            .engine
            .store()
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                root,
                handle,
                hya_store::ResidentArchive {
                    actor,
                    generation: handoff.generation,
                    doc: handoff.doc,
                    degraded: handoff.degraded,
                    reason,
                    note,
                    member_status: status,
                    report: None,
                    check_gate: false,
                    expected_work: None,
                },
                ending.cause(),
            )
            .await?;
        self.engine
            .publish_resident_finalization((envelopes, admissions));
        Ok(())
    }

    async fn resolve_main_activation(&self, root: SessionId) -> Result<MainActivation, CoreError> {
        let projection = self.engine.read_projection(root).await?;
        let session = &projection.session;
        let invalid =
            || CoreError::Invalid(format!("root session {root} lacks activation context"));
        let base = AgentSpec {
            name: session.agent.clone().ok_or_else(invalid)?,
            model: session.model.clone().ok_or_else(invalid)?,
            workdir: session.workdir.clone().ok_or_else(invalid)?.into(),
            system_prompt: String::new(),
            reasoning: None,
        };
        let binding = self
            .engine
            .bind_session_runtime(root, &base.workdir)
            .await?;
        let agent = self
            .engine
            .agent_spec_for_binding(&binding, &base, base.name.as_str())?;
        let agents = self
            .engine
            .agent_roster_for_binding(&binding, base.name.as_str())?;
        let resources = self
            .engine
            .agent_resource_policy_for_binding(&binding, base.name.as_str())?;
        self.engine.ensure_root_registered(root).await?;
        Ok(MainActivation {
            agent,
            binding,
            agents,
            resources,
            guidance: None,
        })
    }

    /// Graceful stop (SIGINT/SIGTERM, user cancel, end of a one-shot run,
    /// `serve` shutdown): cancel every in-flight turn in every session with
    /// `cause` and wait up to `deadline` for them to close their messages
    /// ([`SessionEngine::drain_turns`]), then make every team member terminal
    /// and park each lead. New turns are refused from the first step on.
    pub async fn drain(&self, cause: FinishCause, deadline: Duration) -> TurnDrainReport {
        self.engine.begin_drain(cause);
        let endings = async {
            let teams: Vec<Arc<TeamActor>> = self.teams().values().cloned().collect();
            let mut roots = teams.iter().map(|team| team.root).collect::<Vec<_>>();
            for team in self
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
            {
                if !roots.contains(&team.root) {
                    roots.push(team.root);
                }
            }
            let mut orphans = Vec::new();
            match self.engine.store().active_actor_ids().await {
                Ok(actors) => {
                    for actor in actors {
                        if self.team_with_resident(actor).is_some() {
                            continue;
                        }
                        match self.engine.session_lineage(actor).await {
                            Ok((root, _)) => match self.engine.read_projection_shared(root).await {
                                Ok(projection) => {
                                    if let Some((handle, _)) =
                                        projection.team.roster.iter().find(|(handle, entry)| {
                                            handle.as_str() != scope::ROOT_HANDLE
                                                && entry.session == actor
                                        })
                                    {
                                        orphans.push((root, handle.clone(), actor));
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(%root, %error, "shutdown could not read orphan roster")
                                }
                            },
                            Err(error) => {
                                tracing::error!(%actor, %error, "shutdown could not resolve orphan root")
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "shutdown could not read active actor claims")
                }
            }
            let mut done = Vec::new();
            for team in teams {
                done.extend(team.kill(Ending::Shutdown).await);
            }
            for root in &roots {
                for team in self.episode_teams(*root) {
                    done.extend(team.kill(Ending::Shutdown).await);
                }
            }
            for result in futures::future::join_all(done.into_iter().map(await_episode)).await {
                if let Err(error) = result {
                    tracing::error!(%error, "resident shutdown ending failed");
                }
            }
            // Only actors already orphaned at entry are reconciled here. A
            // failed write in this drain leaves an orphan for the next request.
            for (root, handle, actor) in orphans {
                if let Err(error) = self
                    .end_orphan(root, &handle, actor, &Ending::Shutdown)
                    .await
                {
                    tracing::error!(%root, %actor, %error, "resident shutdown orphan reconciliation failed");
                }
            }
        };
        // The turn drain and each episode's own cancellation deadline run
        // together, so shutdown pays one drain deadline, not two in series.
        let (report, ()) = tokio::join!(self.engine.drain_turns(cause, deadline), endings);
        self.listener_stop.cancel();
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(listener) = listener
            && let Err(error) = listener.await
        {
            tracing::error!(%error, "resident bus listener failed");
        }
        report
    }

    /// Number of in-memory member episodes (the claim-less lead is excluded).
    /// A failed terminal write releases this ownership even while its durable
    /// claim remains active for later reconciliation.
    #[must_use]
    pub fn episode_count(&self) -> usize {
        self.episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether the resident slot for `session` has work in flight: a running
    /// turn, owed mail or directive, or an accepted report still to execute.
    /// `None` when this process tracks no such slot.
    pub(crate) fn member_busy(&self, root: SessionId, session: SessionId) -> Option<bool> {
        let team = self
            .team_with_resident(session)
            .filter(|team| team.root == root)?;
        let state = team.lock();
        let slot = state.residents.get(&session)?;
        Some(
            slot.status == SlotStatus::Busy
                || slot.has_work()
                || slot.report.is_some()
                || slot.ending.is_some()
                || self.engine.turn_active(session),
        )
    }

    /// Whether `session`'s slot is committing its archive (a report executing
    /// or an `archive` in progress): its report mail may already be out while
    /// the archive marker is not.
    pub(crate) fn member_archiving(&self, root: SessionId, session: SessionId) -> bool {
        self.team_with_resident(session)
            .filter(|team| team.root == root)
            .is_some_and(|team| {
                team.lock()
                    .residents
                    .get(&session)
                    .is_some_and(|slot| slot.ending.is_some() || slot.report.is_some())
            })
    }

    /// Whether the user stopped `session`'s slot (see
    /// [`ArchiveReviver::stop_for_user`]).
    pub(crate) fn member_stopped_by_user(&self, root: SessionId, session: SessionId) -> bool {
        self.team_with_resident(session)
            .filter(|team| team.root == root)
            .is_some_and(|team| {
                team.lock()
                    .residents
                    .get(&session)
                    .is_some_and(|slot| slot.user_stopped)
            })
    }

    /// The team `session` belongs to: the one it leads, or the one it is a
    /// member of.
    fn team_of(&self, session: SessionId) -> Option<Arc<TeamActor>> {
        let led = self.teams().get(&session).cloned();
        led.or_else(|| self.team_with_resident(session))
    }

    /// Whether `caller`'s `wait` already reported `member` as stopped at the
    /// turn ending in `last_answer` (so a repeated wait must not wake on it).
    pub(crate) fn wait_stall_reported(
        &self,
        caller: SessionId,
        member: SessionId,
        last_answer: Option<hya_proto::MessageId>,
    ) -> bool {
        let stalls = match self.wait_stalls.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        stalls.get(&(caller, member)) == Some(&last_answer)
    }

    /// Remember that `caller`'s `wait` reported `member` stopped at
    /// `last_answer`.
    pub(crate) fn record_wait_stall(
        &self,
        caller: SessionId,
        member: SessionId,
        last_answer: Option<hya_proto::MessageId>,
    ) {
        let mut stalls = match self.wait_stalls.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        stalls.insert((caller, member), last_answer);
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

    /// Request an archive and wait for the episode's own ending. Dropping
    /// this wait never cancels the episode task.
    pub async fn end_member(&self, root: SessionId, handle: &str) -> Result<(), CoreError> {
        let live = self
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find_map(|(session, team)| {
                (team.root == root
                    && team
                        .lock()
                        .residents
                        .get(session)
                        .is_some_and(|slot| slot.handle == handle))
                .then_some(*session)
            });
        if let Some(child) = live {
            self.stop_and_archive_live(root, handle, child, "Workflow run cancelled")
                .await?;
            return Ok(());
        }
        let projection = self.engine.read_projection(root).await?;
        if projection.team.archived.contains_key(handle) {
            return Ok(());
        }
        let child = projection
            .team
            .roster
            .get(handle)
            .ok_or_else(|| CoreError::Invalid(format!("unknown resident `{handle}`")))?
            .session;
        self.stop_and_archive_live(root, handle, child, "Workflow run cancelled")
            .await?;
        Ok(())
    }

    /// Projection-only report-gate check (ADR-0015): the handle must be live,
    /// its inbox drained, and it must have no live children. The tool surface
    /// calls this for fast feedback; the episode re-checks it at rest.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] for each gate failure.
    pub async fn report_gate(&self, root: SessionId, handle: &str) -> Result<String, CoreError> {
        let canonical = handle.to_string();
        self.report_gate_state(root, &canonical).await?;
        Ok(canonical)
    }

    /// Shared gate body over an already-canonical handle; returns the
    /// projection it validated.
    async fn report_gate_state(
        &self,
        root: SessionId,
        canonical: &str,
    ) -> Result<hya_proto::Projection, CoreError> {
        report_gate_projection(&self.engine, root, canonical).await
    }

    /// The `report` tool entry (ADR-0015): gate feedback now, archive once the
    /// actor is at rest. Works mid-turn — the accepted report parks on the
    /// slot and executes after the current turn ends (gate re-checked there).
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] for gate failures or a dead handle.
    pub async fn submit_report(
        &self,
        root: SessionId,
        handle: &str,
        outcome: ReportOutcome,
        report: String,
    ) -> Result<(), CoreError> {
        // Fast gate feedback against durable truth (unread mail / children).
        let canonical = self.report_gate(root, handle).await?;
        let team =
            self.teams().get(&root).cloned().ok_or_else(|| {
                CoreError::Invalid(format!("resident `{handle}` has no live team"))
            })?;
        {
            let mut state = team.lock();
            let slot = state
                .residents
                .iter_mut()
                .find(|(_, slot)| slot.handle == canonical)
                .map(|(_, slot)| slot)
                .ok_or_else(|| CoreError::Invalid(format!("resident `{handle}` is not live")))?;
            if slot.is_main {
                return Err(CoreError::Invalid(
                    "the main actor cannot report".to_string(),
                ));
            }
            slot.report = Some(PendingReport { outcome, report });
            slot.notify.notify_one();
        }
        Ok(())
    }

    /// The `archive` tool (0.41.0, replaces `kill`): stop one of `caller`'s
    /// subagents and archive it. Any live descendants of the target archive
    /// first (deepest first). For each member the in-flight turn is cancelled
    /// with `cause: archived` and waited for (up to [`DRAIN_DEADLINE`]; a turn
    /// still open then is closed like a drain straggler), then the member
    /// archives with reason `archived_by_parent`: degraded handoff, member row
    /// cancelled, claim released, off the live roster. Its session log and
    /// channel history stay readable, and mail to its handle (or its DM
    /// channel) wakes it under the same handle and session.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] with an actionable message when the target is
    /// unknown (listing the caller's live subagents), already archived, the
    /// team lead, or not a descendant of the caller.
    pub async fn archive_member(
        &self,
        caller: SessionId,
        target: &str,
        reason: &str,
    ) -> Result<ArchiveReceipt, CoreError> {
        let note = if reason.trim().is_empty() {
            "archived by parent".to_string()
        } else {
            format!("archived by parent: {}", reason.trim())
        };
        self.archive_member_noted(caller, target, note).await
    }

    /// [`archive_member`](Self::archive_member) with the archive `note`
    /// recorded on the member row and handoff, naming who archived it.
    async fn archive_member_noted(
        &self,
        caller: SessionId,
        target: &str,
        note: String,
    ) -> Result<ArchiveReceipt, CoreError> {
        let (root, _) = self.engine.session_lineage(caller).await?;
        let caller_path = self.engine.resolve_handle(root, caller).await?;
        let projection = self.engine.read_projection(root).await?;
        let resolved = resolve_member_target(&projection, root, &caller_path, target)?;
        if resolved.session == root || resolved.handle == scope::ROOT_HANDLE {
            return Err(CoreError::Invalid(
                "`main` is the team lead; the lead is never archived".to_string(),
            ));
        }
        if !resolved.live {
            return Err(CoreError::Invalid(format!(
                "`{}` is already archived; send it mail (`send` to `{}`) to wake it",
                resolved.handle, resolved.handle
            )));
        }
        let prefix = format!("{}{}", resolved.handle, scope::PATH_SEPARATOR);
        if !resolved
            .handle
            .starts_with(&format!("{caller_path}{}", scope::PATH_SEPARATOR))
        {
            let parent = scope::parent_path(&resolved.handle).unwrap_or(scope::ROOT_HANDLE);
            return Err(CoreError::Invalid(format!(
                "`{}` is not one of your subagents; only its parent `{parent}` or an ancestor can archive it",
                resolved.handle
            )));
        }
        let mut descendants: Vec<(String, SessionId)> = projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect();
        descendants.sort_by_key(|(path, _)| {
            std::cmp::Reverse(path.matches(scope::PATH_SEPARATOR).count())
        });
        for (handle, session) in &descendants {
            self.stop_and_archive_live(root, handle, *session, &note)
                .await?;
        }
        let cancelled_turn = self
            .stop_and_archive_live(root, &resolved.handle, resolved.session, &note)
            .await?;
        Ok(ArchiveReceipt {
            handle: resolved.handle,
            session: resolved.session,
            cancelled_turn,
            descendants: descendants.into_iter().map(|(handle, _)| handle).collect(),
        })
    }

    /// Stop one live member and archive it (`archived_by_parent`); `true`
    /// when an in-flight turn was cancelled.
    async fn stop_and_archive_live(
        &self,
        root: SessionId,
        handle: &str,
        child: SessionId,
        note: &str,
    ) -> Result<bool, CoreError> {
        if child == root || handle == scope::ROOT_HANDLE {
            return Err(CoreError::Invalid("the team lead is never archived".into()));
        }
        let write = self.claim_writes.lock().await;
        let requested = self.team_with_resident(child).and_then(|team| {
            let mut state = team.lock();
            state.residents.get_mut(&child).map(|slot| {
                let cancelled = self.engine.turn_active(child);
                (
                    slot.request_end(&self.engine, child, Ending::Archived(note.into())),
                    cancelled,
                )
            })
        });
        if let Some((done, cancelled)) = requested {
            drop(write);
            return await_episode(done).await.map(|()| cancelled);
        }
        let handoff = self.engine.degraded_terminal_handoff(child).await;
        let (_, envelopes, admissions) = self
            .engine
            .store()
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                root,
                handle,
                hya_store::ResidentArchive {
                    actor: child,
                    generation: handoff.generation,
                    doc: handoff.doc,
                    degraded: handoff.degraded,
                    reason: ArchiveReason::ArchivedByParent,
                    member_status: MemberRunStatus::Cancelled,
                    note: note.into(),
                    report: None,
                    check_gate: false,
                    expected_work: None,
                },
                FinishCause::Archived,
            )
            .await?;
        self.engine
            .publish_resident_finalization((envelopes, admissions));
        Ok(false)
    }

    /// Revive one archived direct child of `parent` with `body` as the wake
    /// prompt (ADR-0015 §4). Re-resolves the episode runtime from the CURRENT
    /// binding, seeds the fresh episode with the latest handoff, re-registers
    /// the agent under a new claim epoch, and marks the restart durably.
    pub async fn revive_child(
        &self,
        root: SessionId,
        parent: &str,
        child_handle: &str,
        body: String,
    ) -> Result<(), CoreError> {
        let projection = self.engine.read_projection(root).await?;
        let canonical = child_handle.to_string();
        let entry = projection.team.archived.get(&canonical).ok_or_else(|| {
            CoreError::Invalid(format!("`{child_handle}` is not an archived agent"))
        })?;
        if scope::parent_path(&canonical) != Some(parent) {
            return Err(CoreError::Invalid(format!(
                "`{child_handle}` is not a direct child of `{parent}`"
            )));
        }
        let child = entry.session;
        let stable = entry.agent_type.clone();

        // Re-resolve from the current runtime; a vanished stable id fails the
        // revive closed instead of silently downgrading the agent.
        let child_projection = self.engine.read_projection(child).await?;
        let workdir = child_projection
            .session
            .workdir
            .clone()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| CoreError::Invalid(format!("session not found: {child}")))?;
        let binding = self.engine.bind_session_runtime(child, &workdir).await?;
        let agents = self
            .engine
            .agent_roster_for_binding(&binding, stable.as_str())?;
        let resources = self
            .engine
            .agent_resource_policy_for_binding(&binding, stable.as_str())?;
        let base = AgentSpec {
            name: stable.clone(),
            model: child_projection
                .session
                .model
                .clone()
                .unwrap_or_else(|| ModelRef::new("fake")),
            system_prompt: String::new(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let agent = self
            .engine
            .agent_spec_for_binding(&binding, &base, stable.as_str())?;

        // Seed under the new claim, in the registration transaction, before
        // arming. A failed competing revival must leave no transcript writes.
        let mut events = Vec::new();
        if let Some(handoff) = child_projection.session.handoff.clone() {
            let message = hya_proto::MessageId::new();
            let part = hya_proto::PartId::new();
            events.extend([
                Event::MessageStarted {
                    session: child,
                    message,
                    role: hya_proto::Role::System,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session: child,
                    message,
                    part,
                },
                Event::TextDelta {
                    session: child,
                    message,
                    part,
                    delta: format!(
                        "PRIOR HANDOFF (generation {})\n{}",
                        handoff.generation, handoff.doc
                    ),
                },
                Event::TextEnd {
                    session: child,
                    message,
                    part,
                },
                Event::MessageFinished {
                    session: child,
                    message,
                    role: hya_proto::Role::System,
                    finish: hya_proto::FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
            ]);
        }

        let parent_path = scope::parent_path(&canonical).unwrap_or(scope::ROOT_HANDLE);
        let leaf = scope::leaf(&canonical).to_string();
        let arming = format!("[mail from {parent}] {body}");
        self.register_episode(
            root,
            child,
            parent_path.to_string(),
            leaf,
            agent,
            ResidentRuntimeContext::Resolved {
                binding,
                agents,
                resources,
            },
            ResidentActivation {
                events,
                parent_claim: None,
                initial: Some(arming),
                guidance: None,
                route: None,
                sidecar_factory: None,
            },
        )
        .await?;

        // Durable restart marker under the new incarnation's epoch.
        let epoch = self
            .slot_claim_epoch(root, child)
            .unwrap_or(ActorEpoch::INITIAL);
        self.engine
            .emit_for_actor(
                None,
                root,
                Event::AgentRestarted {
                    session: root,
                    handle: canonical,
                    child,
                    epoch,
                },
            )
            .await?;
        Ok(())
    }

    fn slot_claim_epoch(&self, root: SessionId, session: SessionId) -> Option<ActorEpoch> {
        self.teams().get(&root).and_then(|team| {
            let state = team.lock();
            state
                .residents
                .get(&session)
                .and_then(|slot| slot.claim.as_ref().map(|claim| claim.epoch))
        })
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
        self.engine
            .ensure_root_registered_for_actor(root, actor_claim)
            .await?;
        self.team_for_with_main(
            root,
            Some(MainActivation {
                agent,
                binding,
                agents,
                resources,
                guidance,
            }),
        )
        .await?;
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
        let (session, handle, _) = self
            .spawn_resident_inner(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(
                    ResidentRegistration::Armed(directive),
                    parent_claim,
                    guidance,
                ),
            )
            .await?;
        Ok((session, handle))
    }

    /// [`spawn_resident`](Self::spawn_resident) for a `task` spawn: the handle
    /// prefix is the sanitized `subagent_type` (the resolved agent id the call
    /// named, see [`hya_tool::sanitize_handle_prefix`]) rather than the spec's
    /// agent name, which an inline overlay may replace. The handle becomes
    /// `<parent path>/<subagent_type>-<operator>`; see [`crate::handle_naming`].
    /// The member row on the parent log carries the call's description and id.
    ///
    /// # Errors
    /// Every [`spawn_resident`](Self::spawn_resident) failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_resident_typed(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        directive: String,
        task: TaskSpawnOrigin,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String), CoreError> {
        let (session, handle, _) = self
            .spawn_resident_inner(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(
                    ResidentRegistration::Armed(directive),
                    parent_claim,
                    guidance,
                )
                .with_task(task),
            )
            .await?;
        Ok((session, handle))
    }

    /// Create and register one resident without an implicit first turn.
    ///
    /// The caller must deliver work through durable mail after this returns.
    /// `registration_directive` is provenance only and must not contain the work
    /// payload or input values. Returns the Session, handle, and actual member id.
    pub async fn spawn_resident_parked(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        registration_directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        self.spawn_resident_inner(
            parent,
            agent,
            resolved,
            ResidentSpawnContext::new(
                ResidentRegistration::Parked(registration_directive),
                parent_claim,
                guidance,
            ),
        )
        .await
    }

    async fn spawn_resident_inner(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        context: ResidentSpawnContext,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        let ResidentSpawnContext {
            registration,
            parent_claim,
            guidance,
            task,
        } = context;
        let prefix = hya_tool::sanitize_handle_prefix(
            task.as_ref()
                .map_or(agent.name.as_str(), |task| task.subagent_type.as_str()),
        );
        let (registration_directive, initial) = match registration {
            ResidentRegistration::Armed(directive) => (directive.clone(), Some(directive)),
            ResidentRegistration::Parked(directive) => (directive, None),
        };
        let (binding, agents, resources, sidecar_factory) = resolved;
        let (root, parent_depth) = self.engine.session_lineage(parent).await?;
        let session = SessionId::new();
        let parent_projection = self.engine.read_projection(parent).await?;
        let mut events = vec![Event::SessionCreated {
            session,
            parent: Some(parent),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: parent_projection.session.project,
            kind: parent_projection.session.kind,
        }];
        let parent_path = self
            .engine
            .resolve_handle(root, parent)
            .await
            .unwrap_or_else(|_| scope::ROOT_HANDLE.to_string());
        let leaf = self.engine.mint_member_leaf(root, &prefix).await;
        let handle = scope::join_path(&parent_path, &leaf);
        let member = MemberId::new();
        let description = task
            .as_ref()
            .map(|task| task.description.trim())
            .filter(|description| !description.is_empty())
            .map_or_else(
                || registration_directive.chars().take(80).collect(),
                ToString::to_string,
            );
        let tool_call = task.as_ref().and_then(|task| task.tool_call);
        events.push(Event::MemberSpawned {
            session: parent,
            member,
            child: Some(session),
            subagent_type: agent.name.clone(),
            description,
            depth: parent_depth.saturating_add(1),
            directive: registration_directive,
            tool_call,
        });
        self.register_episode(
            root,
            session,
            parent_path,
            leaf,
            agent,
            ResidentRuntimeContext::Resolved {
                binding,
                agents,
                resources,
            },
            ResidentActivation {
                events,
                parent_claim,
                initial,
                guidance,
                route: None,
                sidecar_factory,
            },
        )
        .await?;
        Ok((session, handle, member))
    }

    /// Replace one idle resident's in-process guidance and request-local route
    /// before its next durable mail activation.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] when the resident is unregistered or busy.
    pub(crate) async fn set_resident_workflow_activation(
        &self,
        session: SessionId,
        guidance: Arc<str>,
        route: Option<WorkflowTurnRoute>,
    ) -> Result<(), CoreError> {
        let (root, _) = self.engine.session_lineage(session).await?;
        let team = self
            .teams()
            .get(&root)
            .cloned()
            .ok_or_else(|| CoreError::Invalid("resident team is not registered".to_string()))?;
        let mut state = team.lock();
        let slot = state
            .residents
            .get_mut(&session)
            .ok_or_else(|| CoreError::Invalid("resident session is not registered".to_string()))?;
        if slot.status != SlotStatus::Idle || slot.has_work() {
            return Err(CoreError::Invalid(
                "resident Workflow activation cannot change during active work".to_string(),
            ));
        }
        slot.guidance = Some(guidance);
        slot.route = route;
        Ok(())
    }

    /// Clear one idle resident's request-local workflow route (ADR-0017).
    ///
    /// Stage routes hold a route-recorder sender clone; the parked actor keeps
    /// living after the Stage settles, so the route MUST be dropped here or the
    /// owning run's route-persistence task waits on an open channel forever.
    pub(crate) async fn clear_resident_workflow_route(
        &self,
        session: SessionId,
    ) -> Result<(), CoreError> {
        let (root, _) = self.engine.session_lineage(session).await?;
        let team = self
            .teams()
            .get(&root)
            .cloned()
            .ok_or_else(|| CoreError::Invalid("resident team is not registered".to_string()))?;
        let mut state = team.lock();
        let slot = state
            .residents
            .get_mut(&session)
            .ok_or_else(|| CoreError::Invalid("resident session is not registered".to_string()))?;
        if slot.status != SlotStatus::Idle || slot.has_work() {
            return Err(CoreError::Invalid(
                "resident Workflow route cannot clear during active work".to_string(),
            ));
        }
        slot.route = None;
        Ok(())
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
        let binding = self
            .engine
            .bind_session_runtime(session, &agent.workdir)
            .await?;
        let parent_path = self.engine.parent_agent_path(root, session).await;
        let leaf = scope::leaf(&handle).to_string();
        self.register_episode(
            root,
            session,
            parent_path,
            leaf,
            agent,
            ResidentRuntimeContext::Bound(binding),
            ResidentActivation {
                events: Vec::new(),
                parent_claim: None,
                initial,
                guidance: None,
                route: None,
                sidecar_factory: None,
            },
        )
        .await
    }

    /// Register a resident at an **explicit** canonical path (ADR-0015 revive):
    /// lineage-derived parents disappear from the roster once archived, so
    /// revival must pass the archived entry's own parent path and leaf.
    #[allow(clippy::too_many_arguments)]
    async fn register_episode(
        &self,
        root: SessionId,
        session: SessionId,
        parent_path: String,
        leaf: String,
        agent: AgentSpec,
        runtime: ResidentRuntimeContext,
        activation: ResidentActivation,
    ) -> Result<(), CoreError> {
        let ResidentActivation {
            mut events,
            parent_claim,
            initial,
            guidance,
            route,
            sidecar_factory,
        } = activation;
        let (binding, agents, resources) = match runtime {
            ResidentRuntimeContext::Bound(binding) => (binding, None, None),
            ResidentRuntimeContext::Resolved {
                binding,
                agents,
                resources,
            } => (binding, Some(agents), Some(resources)),
        };
        let handle = scope::join_path(&parent_path, &leaf);
        // The store resolves the unit's group channel under the writer lock;
        // simultaneous first siblings must join the same broadcast channel.
        let projection = self.engine.read_projection(root).await?;
        let group_channel = hya_proto::mint_channel_id(ChannelKind::Group);
        let dm_channel = hya_proto::mint_channel_id(ChannelKind::Dm);
        events.extend([
            Event::AgentRegistered {
                session: root,
                agent_session: session,
                handle: leaf,
                parent: Some(parent_path.clone()),
                agent_type: agent.name.clone(),
                mode: SubagentMode::Resident,
            },
            // The unit's leader-only broadcast channel: minted with the
            // leader as founding member on first registration, joined
            // by every child thereafter.
            Event::ChannelCreated {
                session: root,
                channel: group_channel,
                kind: ChannelKind::Group,
                unit: Some(parent_path.clone()),
                members: vec![parent_path.clone(), handle.clone()],
            },
            // The persistent parent-child DM pair: the revival address
            // that outlives archive cycles.
            Event::ChannelCreated {
                session: root,
                channel: dm_channel,
                kind: ChannelKind::Dm,
                unit: None,
                members: vec![parent_path.clone(), handle.clone()],
            },
        ]);
        let initial = initial.filter(|text| !text.trim().is_empty());
        let has_initial = initial.is_some();
        if let Some(text) = initial {
            events.extend(self.engine.prepare_initial_resident_prompt(session, text));
        }
        let channel_policy =
            ChannelPolicy::from_binding(&binding)?.snapshot_for(agent.name.as_str());
        let mut team = self.team_for(root).await?;
        let mut write = self.claim_writes.lock().await;
        loop {
            let previous = self.team_with_resident(session).and_then(|team| {
                team.lock()
                    .residents
                    .get(&session)
                    .map(|slot| slot.done.subscribe())
            });
            let Some(done) = previous else {
                break;
            };
            if !projection.team.archived.contains_key(&handle) {
                return Err(CoreError::Invalid(
                    "resident already has a live episode".into(),
                ));
            }
            drop(write);
            // A failed old write still leaves an orphan; the registration's
            // existing store fence determines whether it can be revived.
            let _ = await_episode(done).await;
            team = self.team_for(root).await?;
            write = self.claim_writes.lock().await;
        }
        let (claim, envelopes) = self
            .engine
            .store()
            .commit_resident_registration(session, self.owner_run_id, parent_claim.as_ref(), events)
            .await?;
        self.engine
            .update_session_channel_policy(session, channel_policy);
        let parent_session = projection
            .team
            .roster
            .get(&parent_path)
            .map_or(root, |entry| entry.session);
        let notify = Arc::new(Notify::new());
        let initial_prompt = envelopes.iter().find_map(|envelope| match envelope.event {
            Event::MessageStarted {
                session: id,
                message,
                role: hya_proto::Role::User,
                ..
            } if id == session => Some(message),
            _ => None,
        });
        let (ready, initialized) = oneshot::channel();
        let init = EpisodeInit {
            binding: binding.clone(),
            agent: agent.name.clone(),
            parent: parent_session,
            envelopes,
            ready,
        };
        team.install_episode(
            session,
            Episode {
                handle: handle.clone(),
                agent,
                binding,
                agents,
                resources,
                guidance,
                route,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending: has_initial,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: Some(claim),
                cancel: team.cancel.child_token(),
                ending: None,
                done: watch::channel(None).0,
                initial_prompt,
                cursor: 0,
                last_activity: None,
                last_heartbeat_emit: None,
                notify: notify.clone(),
            },
            Some(init),
        );
        drop(write);
        initialized
            .await
            .map_err(|_| CoreError::Invalid("resident initialization task exited".into()))??;
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
        let team = self.team_for(root).await?;
        let _write = self.claim_writes.lock().await;
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
        ) || projection
            .team
            .inboxes
            .get(&handle)
            .is_some_and(|inbox| u64::try_from(inbox.len()).unwrap_or(u64::MAX) > cursor);
        if team
            .lock()
            .residents
            .contains_key(&recovered.claim.actor_id)
        {
            return Err(CoreError::Invalid("resident already has an episode".into()));
        }
        let actor = self
            .engine
            .read_projection(recovered.claim.actor_id)
            .await?;
        let initial_prompt = matches!(disposition, ResidentRecovery::Queued { .. })
            .then(|| actor.session.messages.last())
            .flatten()
            .filter(|message| message.role == hya_proto::Role::User)
            .map(|message| message.id);
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
                initial_prompt,
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

async fn await_episode(mut done: EpisodeDone) -> Result<(), CoreError> {
    loop {
        if let Some(result) = done.borrow().clone() {
            return result;
        }
        done.changed()
            .await
            .map_err(|_| CoreError::Invalid("episode exited without its ending result".into()))?;
    }
}
