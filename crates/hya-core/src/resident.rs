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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use hya_bundle::{ChannelCapability, ChannelParticipantRole, ChannelTemplateKind};
use hya_proto::{
    ActorClaim, ArchiveReason, ChannelKind, Event, FinishCause, FinishReason, MailEndpoint,
    MemberId, MemberRunStatus, OwnerRunId, ReportOutcome, RosterStatus, SessionId, SubagentMode,
    ToolCallId, scope,
};
use hya_tool::{AgentDef, ArchiveReceipt, ResolvedTool};
use tokio::sync::{Notify, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

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

/// Completion of one explicitly armed resident activation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ActivationResult {
    Settled {
        final_text: String,
    },
    Reported {
        outcome: ReportOutcome,
        text: String,
    },
    Ended {
        reason: ArchiveReason,
        status: MemberRunStatus,
        note: String,
    },
}

/// A single activation's completion; cancelling a wait leaves the episode owned.
pub(crate) struct ActivationWaiter {
    done: oneshot::Receiver<Result<ActivationResult, CoreError>>,
}

impl ActivationWaiter {
    pub(crate) async fn wait(&mut self) -> Result<ActivationResult, CoreError> {
        (&mut self.done).await.map_err(|_| {
            CoreError::Invalid("episode exited without its activation result".into())
        })?
    }
}

struct ActivationCompletion {
    boundary: usize,
    done: oneshot::Sender<Result<ActivationResult, CoreError>>,
}

struct WorkflowActivation {
    resolved: (AgentSpec, ResolvedResidentRuntime),
    guidance: Arc<str>,
    route: Option<WorkflowTurnRoute>,
    done: oneshot::Sender<Result<ActivationResult, CoreError>>,
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
    activation: Option<ActivationCompletion>,
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
    Shutdown(FinishCause),
}

impl Ending {
    fn cause(&self) -> FinishCause {
        match self {
            Self::Archived(_) | Self::Teardown => FinishCause::Archived,
            Self::Shutdown(cause) => *cause,
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
            Self::Shutdown(cause) => (
                ArchiveReason::Shutdown,
                MemberRunStatus::Cancelled,
                match cause {
                    FinishCause::UserCancel => "stopped: cancelled by the user",
                    FinishCause::LeaderFailed => "stopped: the team lead failed",
                    _ => "stopped: shutdown",
                }
                .into(),
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
    registrations: usize,
    // Terminal mail keeps main alive between episode removal and bus routing.
    routing_mail: HashSet<u64>,
    queued_wakes: usize,
    /// Active Workflow/headless root drivers suppress autonomous root turns.
    root_driver: Arc<RootDriverState>,
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

/// Resolved root context for installing a team after batch registration.
#[derive(Clone)]
pub struct MainActivation {
    /// Resolved team-root agent, from the triggering binding.
    pub agent: AgentSpec,
    /// Captured root runtime binding.
    pub binding: TurnBinding,
    /// Root's authorized spawn roster.
    pub agents: Arc<[AgentDef]>,
    /// Root's resource policy.
    pub resources: AgentResourcePolicy,
    /// In-process triggering-turn guidance.
    pub guidance: Option<Arc<str>>,
}

/// One fully resolved member of an atomic `task` batch.
pub struct TaskResidentSpawn {
    /// Resolved member specification.
    pub agent: AgentSpec,
    /// Captured binding, roster, policy and executable sidecar factory.
    pub runtime: ResolvedResidentRuntime,
    /// Initial directive owned by the new episode.
    pub directive: String,
    /// Source task call and display label.
    pub task: TaskSpawnOrigin,
    /// In-process triggering-turn guidance.
    pub guidance: Option<Arc<str>>,
}

struct PreparedResident {
    reservation: Option<crate::handle_naming::HandleReservation>,
    session: SessionId,
    parent_path: String,
    leaf: String,
    agent: AgentSpec,
    runtime: ResidentRuntimeContext,
    activation: ResidentActivation,
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
    /// The session's single-active-turn claim, taken atomically with the
    /// decision to run and held until the turn ends.
    lease: TurnLease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnOutcome {
    NotStarted,
    Finished {
        inbox_through: u64,
    },
    Cancelled {
        cause: Option<FinishCause>,
        mail_owed: bool,
    },
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
    directive: String,
    initial: Option<String>,
    parent_claim: Option<ActorClaim>,
    guidance: Option<Arc<str>>,
    /// The `task` call behind the spawn, when there is one.
    task: Option<TaskSpawnOrigin>,
}

impl ResidentSpawnContext {
    fn new(
        directive: String,
        initial: Option<String>,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Self {
        Self {
            directive,
            initial,
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
    tasks: TaskTracker,
    events: tokio::sync::mpsc::UnboundedSender<TeamEvent>,
}

enum TeamEvent {
    #[cfg(test)]
    Flush(oneshot::Sender<()>),
    Envelope(Box<hya_proto::Envelope>),
    Recover,
}

impl TeamEvent {
    fn keeps_team_live(&self) -> bool {
        match self {
            Self::Envelope(envelope) => matches!(envelope.event, Event::MailSent { .. }),
            Self::Recover => true,
            #[cfg(test)]
            Self::Flush(_) => false,
        }
    }
}

/// Keeps a selected team alive across claim writes and initialization.
struct TeamRegistration(Arc<TeamActor>);

impl TeamRegistration {
    // Caller holds the supervisor map lock, excluding retirement.
    fn new(team: Arc<TeamActor>) -> Self {
        team.lock().registrations += 1;
        Self(team)
    }
}

impl std::ops::Deref for TeamRegistration {
    type Target = Arc<TeamActor>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TeamRegistration {
    fn drop(&mut self) {
        self.0.lock().registrations -= 1;
        if let Some(supervisor) = self.0.supervisor.upgrade() {
            supervisor.retire_team(&self.0);
        }
    }
}

struct RootDriverState {
    runs: AtomicUsize,
    autonomous: AtomicBool,
}

/// A Workflow or headless driver owns root progress while this guard is held.
pub struct RootDriverActivity(TeamRegistration);

impl Drop for RootDriverActivity {
    fn drop(&mut self) {
        let last = self
            .0
            .lock()
            .root_driver
            .runs
            .fetch_sub(1, Ordering::AcqRel)
            == 1;
        if last
            && let Some(supervisor) = self.0.supervisor.upgrade()
            && let Some(team) = supervisor.teams().get(&self.0.root).cloned()
        {
            let mut state = team.lock();
            state.last_synth_work_seq = state.work_seq;
            if state.root_driver.autonomous.load(Ordering::Acquire)
                && let Some(main) = state.residents.get(&team.root)
                && main.has_work()
            {
                main.notify.notify_one();
            }
        }
    }
}

impl TeamActor {
    fn new(
        root: SessionId,
        engine: Arc<SessionEngine>,
        supervisor: Weak<ResidentSupervisor>,
        root_driver: Arc<RootDriverState>,
        tasks: TaskTracker,
        events: tokio::sync::mpsc::UnboundedSender<TeamEvent>,
    ) -> Self {
        Self {
            root,
            engine,
            cancel: CancellationToken::new(),
            tasks,
            events,
            supervisor,
            state: Mutex::new(TeamState {
                residents: HashMap::new(),
                busy: 0,
                registrations: 0,
                routing_mail: HashSet::new(),
                queued_wakes: 0,
                root_driver,
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
                    activation: None,
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
                    cursor: 0,
                    last_activity: None,
                    last_heartbeat_emit: None,
                    notify: notify.clone(),
                },
            );
        }
        self.tasks.spawn(episode_task(
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
        self.tasks.spawn(episode_task(
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

    fn enqueue(&self, event: TeamEvent) {
        let mut state = self.lock();
        if self.cancel.is_cancelled() {
            return;
        }
        if event.keeps_team_live() {
            state.queued_wakes += 1;
        }
        let _ = self.events.send(event);
    }

    async fn run_events(
        self: Arc<Self>,
        mut events: tokio::sync::mpsc::UnboundedReceiver<TeamEvent>,
    ) {
        loop {
            let event = tokio::select! {
                biased;
                () = self.cancel.cancelled() => break,
                event = events.recv() => event,
            };
            let Some(event) = event else { break };
            let wake = event.keeps_team_live();
            match event {
                TeamEvent::Envelope(envelope) => {
                    if let Event::MailSent { from, to, .. } = &envelope.event {
                        self.on_mail(from, to).await;
                        self.lock().routing_mail.remove(&envelope.seq.0);
                    }
                    if let Some(actor) = envelope.event.session() {
                        self.note_activity(actor).await;
                    }
                }
                TeamEvent::Recover => self.recover().await,
                #[cfg(test)]
                TeamEvent::Flush(done) => {
                    let _ = done.send(());
                }
            }
            if wake {
                self.lock().queued_wakes -= 1;
                if let Some(supervisor) = self.supervisor.upgrade() {
                    supervisor.retire_team(&self);
                }
            }
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
        let lead_failed = st.lead_failed
            || st.root_driver.runs.load(Ordering::Acquire) != 0
            || !st.root_driver.autonomous.load(Ordering::Acquire);
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

    async fn settle_activation(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
    ) -> Result<(), CoreError> {
        let eligible = |slot: &Episode| {
            slot.claim.as_ref() == claim
                && slot.status == SlotStatus::Idle
                && !slot.has_work()
                && !slot.user_stopped
                && slot.ending.is_none()
                && slot.report.is_none()
                && slot
                    .activation
                    .as_ref()
                    .is_some_and(|activation| slot.cursor >= activation.boundary)
        };
        if !self.lock().residents.get(&session).is_some_and(eligible) {
            return Ok(());
        }
        let projection = self.engine.read_projection(session).await?;
        let final_text = projection
            .session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == hya_proto::Role::Assistant)
            .map(|message| {
                message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        let root = self.engine.read_projection(self.root).await?;
        let mut state = self.lock();
        if let Some(slot) = state
            .residents
            .get_mut(&session)
            .filter(|slot| eligible(slot))
            .filter(|slot| {
                root.team.roster.get(&slot.handle).is_some_and(|entry| {
                    entry.resident_work.is_none()
                        && slot.activation.as_ref().is_some_and(|activation| {
                            entry.resident_cursor >= activation.boundary as u64
                        })
                        && root
                            .team
                            .unread_incoming_mail(&slot.handle, entry.resident_cursor)
                            .next()
                            .is_none()
                        && !root
                            .team
                            .roster
                            .keys()
                            .any(|path| scope::parent_path(path) == Some(slot.handle.as_str()))
                })
            })
        {
            slot.route = None;
            if let Some(activation) = slot.activation.take() {
                let _ = activation
                    .done
                    .send(Ok(ActivationResult::Settled { final_text }));
            }
        }
        Ok(())
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
            supervisor.forget_wait_stalls(session);
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
        if st.phase.killed()
            || st.busy != 0
            || st
                .residents
                .values()
                .any(|slot| !slot.is_main && slot.has_work())
            || st.lead_failed
            || st.root_driver.runs.load(Ordering::Acquire) != 0
            || !st.root_driver.autonomous.load(Ordering::Acquire)
        {
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
            if st.root_driver.runs.load(Ordering::Acquire) == 0 {
                st.root_driver.autonomous.store(true, Ordering::Release);
            }
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
    fn request_end(&self, ending: Ending) -> (bool, Vec<EpisodeDone>) {
        let mut state = self.lock();
        let first = !state.phase.killed();
        if first {
            state.phase = TeamPhase::Killed(ending.clone());
        }
        let TeamPhase::Killed(ending) = state.phase.clone() else {
            unreachable!()
        };
        if matches!(ending, Ending::Teardown | Ending::Shutdown(_)) {
            self.engine.cancel_turn(self.root, ending.cause());
        }
        let done = state
            .residents
            .iter_mut()
            .map(|(session, slot)| slot.request_end(&self.engine, *session, ending.clone()))
            .collect();
        self.cancel.cancel();
        (first, done)
    }

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

    /// Run exactly one turn for `session`: coalesce durable mail and the
    /// synthesis directive (main, on quiescence), and every inbox message since the
    /// cursor, then advance the cursor and stream one turn. All of this is coalesced
    /// into a single turn — many queued messages produce one turn, never several.
    ///
    /// Returns `NotStarted` when a wake has nothing left to deliver:
    /// mail that arrived during the lead's own turn and was
    /// already steered into it needs no follow-up turn.
    async fn run_one_turn(
        &self,
        session: SessionId,
        plan: RunPlan,
        sidecar_tools: Arc<[ResolvedTool]>,
    ) -> Result<TurnOutcome, CoreError> {
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
            claim,
            cancel,
            lease,
        } = plan;
        // Admission hooks already belong to the leased turn, including a
        // user stop that arrives before run_turn binds its cancellation token.
        let cancel = lease.bind_cancel(&cancel);
        // Snapshot new inbox mail for this handle (folded before its wake, so it is
        // already visible here). The durable cursor leads the in-memory slot
        // cursor when steer already surfaced mail inside a tool result (or a
        // `wait` returned it) — sync so the wake never re-injects delivered
        // messages as user prompts. The plan's cursor was captured before
        // this sync, so it advances here too.
        let projection = self.engine.read_projection(self.root).await?;
        if !self.owns_slot(session, claim.as_ref()) {
            return Ok(TurnOutcome::NotStarted);
        }
        let mut cursor = cursor;
        let mut durable_cursor = 0;
        if let Some(entry) = projection.team.roster.get(&handle) {
            let durable = usize::try_from(entry.resident_cursor).unwrap_or(usize::MAX);
            durable_cursor = durable;
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
        let mut resumed_prompt = None;
        let mut prompt_answered = false;
        if claim.is_some()
            && let Some(work) = projection
                .team
                .roster
                .get(&handle)
                .and_then(|entry| entry.resident_work)
        {
            cursor = cursor.max(usize::try_from(work.inbox_through).unwrap_or(usize::MAX));
            // The old prompt has already been hooked. Only new mail or a
            // continuation needs hooks; begin_work owns the transcript decision.
            let actor = self.engine.read_projection(session).await?;
            resumed_prompt = Some(
                actor
                    .session
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == hya_proto::Role::User)
                    .ok_or_else(|| {
                        CoreError::Invalid("resident work has no admitted prompt".into())
                    })?
                    .id,
            );
            prompt_answered = actor
                .session
                .messages
                .iter()
                .rev()
                .find(|message| {
                    message.role != hya_proto::Role::Assistant || !message.parts.is_empty()
                })
                .is_some_and(|message| Some(message.id) != resumed_prompt);
        }
        let inbox_len = projection
            .team
            .inboxes
            .get(&handle)
            .map_or(0, |inbox| inbox.len());
        let mail_owed = resumed_prompt.is_some()
            || projection
                .team
                .unread_incoming_mail(&handle, durable_cursor as u64)
                .next()
                .is_some();
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
                        if message.from == handle {
                            return false;
                        }
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

        if resumed_prompt.is_none() && !synth && new_mail.is_empty() {
            if cancel.is_cancelled() {
                return Ok(TurnOutcome::NotStarted);
            }
            if inbox_len > cursor {
                self.engine
                    .emit_for_actor(
                        claim.as_ref(),
                        self.root,
                        Event::MailConsumed {
                            session: self.root,
                            handle: handle.clone(),
                            through: inbox_len as u64,
                        },
                    )
                    .await?;
            }

            let mut st = self.lock();
            if let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            {
                slot.cursor = slot.cursor.max(inbox_len);
            }
            return Ok(TurnOutcome::NotStarted);
        }

        if cancel.is_cancelled() {
            return Ok(TurnOutcome::Cancelled {
                cause: self.engine.turn_cancel_cause(session),
                mail_owed,
            });
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
            return Ok(TurnOutcome::Cancelled {
                cause: self.engine.turn_cancel_cause(session),
                mail_owed,
            });
        }
        let task_label = if synth && is_main {
            "synthesizing".to_string()
        } else if let Some((from, _)) = new_mail.first() {
            format!("mail from {from}")
        } else {
            "working".to_string()
        };
        let mut sections = Vec::new();
        if synth && is_main {
            sections.push(SYNTHESIS_DIRECTIVE.to_string());
        }
        sections.extend(
            new_mail
                .iter()
                .map(|(from, body)| format!("[mail from {from}] {body}")),
        );
        if sections.is_empty() && prompt_answered {
            sections.push("Continue the unfinished work.".into());
        }
        let text = if sections.is_empty() {
            String::new()
        } else {
            // Hooks see the whole wake before any delivery or consumption commits.
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(TurnOutcome::Cancelled { cause: self.engine.turn_cancel_cause(session), mail_owed }),
                text = self.engine.user_text_after_hooks(&binding, agent.name.as_str(), session, sections.join("\n\n")) => text,
            }
        };
        if cancel.is_cancelled() {
            return Ok(TurnOutcome::Cancelled {
                cause: self.engine.turn_cancel_cause(session),
                mail_owed,
            });
        }
        let prompt = match resumed_prompt {
            Some(message) => hya_store::ResidentWorkPrompt::Resume {
                message,
                append: text,
            },
            None => hya_store::ResidentWorkPrompt::Text(text),
        };
        let (admitted_prompt, envelopes) = self
            .engine
            .store()
            .begin_work(
                claim.as_ref(),
                self.root,
                &handle,
                u64::try_from(inbox_len).unwrap_or(u64::MAX),
                prompt,
                task_label,
            )
            .await?;
        self.engine.update_session_channel_policy(
            session,
            channel_policy.snapshot_for(agent.name.as_str()),
        );
        for envelope in envelopes {
            self.engine.publish_envelope(envelope);
        }
        if !self.owns_slot(session, claim.as_ref()) {
            return Ok(TurnOutcome::NotStarted);
        }
        if !is_main {
            self.mark_member_running(session, claim.as_ref()).await;
        }
        self.heartbeat_turn_start(session, claim.as_ref(), &handle)
            .await;
        // Advance the cursor so a follow-up turn never re-injects the same mail.
        {
            let mut st = self.lock();
            let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            else {
                return Ok(TurnOutcome::NotStarted);
            };
            slot.cursor = slot.cursor.max(inbox_len);
        }
        // Exactly one turn, under the team-wide cancel so a budget kill stops it.
        let finish = match (claim.as_ref(), agents, resources) {
            (Some(claim), Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools_for_actor_and_workflow_route(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        TurnRequestContext::new(cancel.clone(), &[], guidance, Some(claim), route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
            (None, Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools_and_workflow_route(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        TurnRequestContext::new(cancel.clone(), &[], guidance, None, route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
            (Some(claim), _, _) => {
                self.engine
                    .run_bound_turn_for_actor_with_workflow_route(
                        session,
                        &agent,
                        binding,
                        TurnRequestContext::new(cancel.clone(), &[], guidance, Some(claim), route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
            (None, _, _) => {
                self.engine
                    .run_bound_turn_with_workflow_route(
                        session,
                        &agent,
                        binding,
                        TurnRequestContext::new(cancel, &[], guidance, None, route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
        };
        let finish = match finish {
            Ok(finish) => finish,
            Err(CoreError::Cancelled) => FinishReason::Cancelled,
            Err(error) => return Err(error),
        };
        // Turn finished: a fresh boundary heartbeat closes the window even
        // when the turn's own engine events never tripped the throttle.
        self.heartbeat_turn_finish(session, claim.as_ref(), &handle)
            .await;
        Ok(if finish == FinishReason::Cancelled {
            let projection = self.engine.read_projection(session).await?;
            let root = self.engine.read_projection(self.root).await?;
            let mail_owed = root.team.roster.get(&handle).is_some_and(|entry| {
                entry.resident_work.is_some()
                    || root
                        .team
                        .unread_incoming_mail(&handle, entry.resident_cursor)
                        .next()
                        .is_some()
            });
            TurnOutcome::Cancelled {
                mail_owed,
                cause: projection
                    .session
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == hya_proto::Role::Assistant)
                    .and_then(|message| message.cause),
            }
        } else {
            TurnOutcome::Finished {
                inbox_through: u64::try_from(inbox_len).unwrap_or(u64::MAX),
            }
        })
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
            // Handle targets are harness notices, never agent-to-agent mail.
            MailEndpoint::Handle(handle) => vec![handle.clone()],
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

    fn take_episode(&self, session: SessionId, claim: Option<&ActorClaim>) -> Option<Episode> {
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
            slot
        };
        self.wake_activated_parent(&done.handle);
        self.forget_episode(session, claim);
        Some(done)
    }

    fn wake_activated_parent(&self, child: &str) {
        if let Some(parent) = scope::parent_path(child)
            && let Some(slot) = self
                .lock()
                .residents
                .values()
                .find(|slot| slot.handle == parent && slot.activation.is_some())
        {
            slot.notify.notify_one();
        }
    }

    fn complete_episode(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        result: Result<(), CoreError>,
    ) {
        if let Some(slot) = self.take_episode(session, claim) {
            slot.done.send_replace(Some(result));
        }
    }

    /// Handle one `MailSent` for this team: charge the message budget, count the
    /// work, and deliver a wake to every resident recipient (never the sender).
    async fn on_mail(&self, from: &str, to: &MailEndpoint) {
        #[cfg(test)]
        {
            let pause = self.supervisor.upgrade().and_then(|supervisor| {
                let mut pause = supervisor
                    .mail_pause
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if pause
                    .as_ref()
                    .is_some_and(|(root, _, _)| *root == self.root)
                {
                    pause.take()
                } else {
                    None
                }
            });
            if let Some((_, entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
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
                    let inbox = projection
                        .team
                        .inboxes
                        .get(&slot.handle)
                        .map_or(&[][..], Vec::as_slice);
                    let inbox_len = inbox.len();
                    let durable = projection.team.roster.get(&slot.handle).map_or(0, |entry| {
                        usize::try_from(entry.resident_cursor).unwrap_or(usize::MAX)
                    });
                    let cursor = slot.cursor.max(durable);
                    let allowed = ChannelPolicy::from_binding(&slot.binding).is_ok_and(|policy| {
                        inbox.iter().skip(cursor).any(|mail| {
                            if mail.from == slot.handle {
                                return false;
                            }
                            if mail.from == scope::HARNESS_HANDLE {
                                return true;
                            }
                            let (kind, role) = recipient_channel_role(
                                &projection,
                                &mail.to,
                                &slot.handle,
                                &mail.from,
                            );
                            resident_delivery_allowed(&policy, slot.agent.name.as_str(), kind, role)
                        })
                    });
                    if !allowed {
                        if !slot.pending && inbox_len > cursor {
                            consumed.push((session, slot.claim, slot.handle.clone(), inbox_len));
                        }
                        continue;
                    }
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
                if slot.activation.is_some() {
                    slot.notify.notify_one();
                }
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
            st.routing_mail.retain(|seq| *seq > projection.last_seq);
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
                    if message.from == slot.handle {
                        return false;
                    }
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
                if slot.activation.is_some() {
                    slot.notify.notify_one();
                }
            }
        }
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor.retire_team(self);
        }
    }
}

fn recipient_channel_role(
    projection: &hya_proto::Projection,
    to: &MailEndpoint,
    recipient: &str,
    sender: &str,
) -> (ChannelTemplateKind, Option<ChannelParticipantRole>) {
    if let MailEndpoint::Channel(channel) = to
        && let Some(state) = projection.team.channels.get(channel)
        && state.kind == ChannelKind::Group
    {
        return (
            ChannelTemplateKind::Unit,
            Some(if state.unit.as_deref() == Some(recipient) {
                ChannelParticipantRole::UnitLeader
            } else {
                ChannelParticipantRole::DirectReports
            }),
        );
    }
    let role = if scope::parent_path(recipient) == Some(sender) {
        Some(ChannelParticipantRole::Child)
    } else if scope::parent_path(sender) == Some(recipient) {
        Some(ChannelParticipantRole::Parent)
    } else {
        None
    };
    let own_dm = match to {
        MailEndpoint::Handle(_) => false,
        MailEndpoint::Channel(channel) => {
            projection.team.channels.get(channel).is_some_and(|state| {
                state.kind == ChannelKind::Dm
                    && state.members.len() == 2
                    && state.members.contains(recipient)
                    && state.members.contains(sender)
            })
        }
    };
    (ChannelTemplateKind::ParentDm, role.filter(|_| own_dm))
}

fn resident_delivery_allowed(
    policy: &ChannelPolicy,
    agent: &str,
    kind: ChannelTemplateKind,
    role: Option<ChannelParticipantRole>,
) -> bool {
    let Some(role) = role else { return false };
    // The direct parent's own DM is the member's mandatory work address.
    if kind == ChannelTemplateKind::ParentDm && role == ChannelParticipantRole::Child {
        return true;
    }
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
    published: oneshot::Receiver<()>,
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
        published,
        ready,
    } = init;
    // No child hook or turn may overtake the committed parent registration.
    let _ = published.await;
    let session = claim.actor_id;
    let created = envelopes.iter().any(|envelope| {
        matches!(envelope.event,
        Event::SessionCreated { session: id, .. } if id == session)
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
    // Only child-log events wait for session-start hooks. Cancellation still
    // publishes them before the episode can write its terminal events.
    for envelope in envelopes {
        team.engine.publish_envelope(envelope);
    }
    let initialize = async {
        if created && let Some(hooks) = team.engine.session_hook_dispatcher(parent) {
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
    task_claim: Option<ActorClaim>,
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
                team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
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
                team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
            }
        }
        if !team.owns_slot(session, task_claim.as_ref()) {
            if let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                tracing::warn!(%session, %error, "stale episode sidecar did not stop");
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
                        team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
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
                    if let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                        tracing::warn!(%session, %error, "idle sidecar did not stop");
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
                    {
                        sidecar_tools = Arc::from([]);
                        sidecar_hooks = None;
                        if let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                            tracing::warn!(%session, %error, "idle sidecar did not stop");
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
                    let loss = async {
                        match loss_token {
                            Some(token) => token.cancelled().await,
                            None => std::future::pending().await,
                        }
                    };
                    let mut sidecar_lost = false;
                    let mut loss_interrupted_turn = false;
                    let result = tokio::select! {
                        biased;
                        () = loss => {
                            sidecar_lost = true;
                            loss_interrupted_turn = team.engine.turn_active(session)
                                && team.engine.turn_cancel_cause(session).is_none()
                                && !team.lock().residents.get(&session).is_some_and(|slot| slot.user_stopped);
                            turn_cancel.cancel();
                            None
                        }
                        _ = cancel.cancelled() => None,
                        result = &mut run => Some(result),
                    };
                    let turn_result = match result {
                        Some(result) => result,
                        None => {
                            // Poll the own turn before any ending writes: it can
                            // hold the connection those writes would need.
                            match tokio::time::timeout(DRAIN_DEADLINE, &mut run).await {
                                Ok(result) => result,
                                Err(_) => {
                                    let cause = team
                                        .lock()
                                        .residents
                                        .get(&session)
                                        .and_then(|slot| slot.ending.as_ref().map(Ending::cause))
                                        .unwrap_or(if sidecar_lost {
                                            FinishCause::Other
                                        } else {
                                            FinishCause::Shutdown
                                        });
                                    let close = async {
                                        team.engine
                                            .close_harness_turn(claim.as_ref(), session, cause)
                                            .await?;
                                        Ok::<(), CoreError>(())
                                    };
                                    // Closing shares the turn's message lock. Keep
                                    // polling its owner until close finishes.
                                    tokio::select! {
                                        result = &mut run => result,
                                        result = close => {
                                            if let Err(error) = result {
                                                tracing::error!(%session, ?claim, %error, "episode straggler close failed");
                                            }
                                            Err(CoreError::Cancelled)
                                        }
                                    }
                                }
                            }
                        }
                    };
                    drop(run);
                    if cancel.is_cancelled()
                        && team
                            .lock()
                            .residents
                            .get(&session)
                            .is_none_or(|slot| slot.ending.is_none())
                    {
                        team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
                    }
                    sidecar_lost |= sidecar_handle
                        .as_ref()
                        .is_some_and(|cached| !cached.is_healthy());
                    if sidecar_lost {
                        sidecar_tools = Arc::from([]);
                        sidecar_hooks = None;
                        let user_stopped = team
                            .lock()
                            .residents
                            .get(&session)
                            .is_some_and(|slot| slot.user_stopped);
                        let interrupted = loss_interrupted_turn
                            || (!user_stopped
                                && matches!(
                                    turn_result,
                                    Ok(TurnOutcome::Cancelled {
                                        cause: None | Some(FinishCause::Other),
                                        ..
                                    }) | Err(CoreError::Cancelled)
                                ));
                        if interrupted
                            && matches!(
                                turn_result,
                                Ok(TurnOutcome::Cancelled { .. }) | Err(CoreError::Cancelled)
                            )
                        {
                            let Some(claim) = claim.as_ref() else {
                                unreachable!("only member episodes have sidecars");
                            };
                            finish_episode(
                                &team,
                                claim,
                                Ending::Failed("activation sidecar lost".into()),
                                &mut sidecar_handle,
                                None,
                            )
                            .await;
                            return;
                        }
                        let ending = team
                            .lock()
                            .residents
                            .get(&session)
                            .is_some_and(|slot| slot.ending.is_some());
                        if !ending && let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                            tracing::warn!(%session, %error, "idle sidecar did not stop");
                        }
                    }
                    if !is_main
                        && matches!(
                            turn_result,
                            Ok(TurnOutcome::Cancelled {
                                mail_owed: true,
                                ..
                            })
                        )
                    {
                        let mut state = team.lock();
                        if let Some(slot) = state
                            .residents
                            .get_mut(&session)
                            .filter(|slot| slot.claim == claim && slot.ending.is_none())
                        {
                            slot.pending = true;
                        }
                    }
                    // Flip the slot idle BEFORE recording the roster activity:
                    // observers gate on "roster says idle" (the report gate),
                    // so that observation must imply the slot is idle too.
                    team.finish_run(session, task_claim.as_ref());
                    match turn_result {
                        Ok(TurnOutcome::NotStarted) => {}
                        Ok(TurnOutcome::Finished { inbox_through }) => {
                            if let Some(claim) = claim.as_ref() {
                                if let Err(error) = team
                                    .engine
                                    .commit_resident_mutation(
                                        claim,
                                        team.root,
                                        vec![
                                            Event::ResidentWorkFinished {
                                                session: team.root,
                                                handle: handle.clone(),
                                                epoch: claim.epoch,
                                                inbox_through,
                                            },
                                            Event::AgentActivityChanged {
                                                session: team.root,
                                                handle,
                                                status: RosterStatus::Idle,
                                                current_task: None,
                                            },
                                        ],
                                    )
                                    .await
                                {
                                    finish_episode(
                                        &team,
                                        claim,
                                        Ending::Failed(format!("work completion error: {error}")),
                                        &mut sidecar_handle,
                                        None,
                                    )
                                    .await;
                                    return;
                                }
                            } else {
                                let _ = team
                                    .record_activity(None, handle, RosterStatus::Idle, None)
                                    .await;
                            }
                        }
                        Ok(TurnOutcome::Cancelled { .. }) | Err(CoreError::Cancelled) => {
                            // A user-stopped episode stays live and durably idle;
                            // a forced ending owns its terminal activity instead.
                            let ending = team
                                .lock()
                                .residents
                                .get(&session)
                                .is_some_and(|slot| slot.ending.is_some());
                            if !ending {
                                let _ = team
                                    .record_activity(
                                        claim.as_ref(),
                                        handle,
                                        RosterStatus::Idle,
                                        None,
                                    )
                                    .await;
                            }
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
                    if let Err(error) = team.settle_activation(session, task_claim.as_ref()).await {
                        let Some(claim) = claim.as_ref() else {
                            return;
                        };
                        finish_episode(
                            &team,
                            claim,
                            Ending::Failed(format!("activation completion error: {error}")),
                            &mut sidecar_handle,
                            None,
                        )
                        .await;
                        return;
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
type MailRoutingPause = (SessionId, Arc<Notify>, Arc<Notify>);

/// Drives resident (long-lived actor) subagents for every team that has one.
///
/// Constructed once per runtime via [`ResidentSupervisor::start`], which spawns a
/// single bus listener. Teams are created lazily as residents/main are registered.
pub struct ResidentSupervisor {
    engine: Arc<SessionEngine>,
    owner_run_id: OwnerRunId,
    teams: Mutex<HashMap<SessionId, Arc<TeamActor>>>,
    /// Root-driver suppression spans killed and replacement team generations.
    root_drivers: Mutex<HashMap<SessionId, Weak<RootDriverState>>>,
    /// Includes episodes on teams already removed by a kill.
    episodes: Mutex<HashMap<SessionId, Arc<TeamActor>>>,
    /// Claim commit + install and orphan Takeover share this boundary across
    /// team generations. Kills never wait for it or for registration hooks.
    claim_writes: tokio::sync::Mutex<()>,
    #[cfg(test)]
    registration_pause: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    #[cfg(test)]
    ending_pause: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    #[cfg(test)]
    activation_lock_pause: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    #[cfg(test)]
    revive_ready_pause: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    #[cfg(test)]
    mail_pause: Mutex<Option<MailRoutingPause>>,
    self_ref: Weak<ResidentSupervisor>,
    tasks: TaskTracker,
    listener_stop: CancellationToken,
    listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// `wait` stall notices already given: (waiting session, member session)
    /// → the member's last assistant message when it was reported stopped.
    /// In-memory only: after a restart a stopped member is reported once more.
    wait_stalls: Mutex<HashMap<(SessionId, SessionId), Option<hya_proto::MessageId>>>,
}

impl Drop for ResidentSupervisor {
    fn drop(&mut self) {
        self.listener_stop.cancel();
        let teams = self
            .teams
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let episodes = self
            .episodes
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for team in teams.values().chain(episodes.values()) {
            if !team.lock().phase.killed() {
                team.request_end(Ending::Shutdown(FinishCause::Shutdown));
            }
        }
    }
}

type EpisodeFinalization = Vec<hya_proto::Envelope>;

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
    let (reason, status, mut report, mut note, check_gate) = match ending {
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
        if let Some((_, report)) = &mut report {
            report.clone_from(&note);
        }
    }
    let (outcome, envelopes) = team
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
        )
        .await?;
    if outcome == hya_store::ResidentArchiveOutcome::Refused {
        team.engine.publish_resident_finalization(envelopes);
        Ok((outcome, Vec::new()))
    } else {
        Ok((outcome, envelopes))
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
    let sidecar_error = stop_sidecar(sidecar).await;
    if committed.is_some()
        && let Some(error) = &sidecar_error
    {
        tracing::error!(session = %claim.actor_id, %error, "reported episode sidecar did not stop");
    }
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
    if let Ok(envelopes) = &result {
        team.lock()
            .routing_mail
            .extend(envelopes.iter().filter_map(|envelope| {
                matches!(envelope.event, Event::MailSent { .. }).then_some(envelope.seq.0)
            }));
    }
    if let Some(slot) = team.take_episode(claim.actor_id, Some(claim)) {
        let activation = slot.activation.map(|activation| {
            let activation_result = result
                .as_ref()
                .map(|envelopes| {
                    if let Some((outcome, text)) =
                        envelopes.iter().find_map(|envelope| match &envelope.event {
                            Event::SubagentReported {
                                child,
                                outcome,
                                report,
                                ..
                            } if *child == claim.actor_id => Some((*outcome, report.clone())),
                            _ => None,
                        })
                    {
                        ActivationResult::Reported { outcome, text }
                    } else {
                        let (reason, status, mut note) = ending.forced_fields();
                        if let Some(summary) =
                            envelopes.iter().find_map(|envelope| match &envelope.event {
                                Event::MemberFinished {
                                    child: Some(child),
                                    summary,
                                    ..
                                } if *child == claim.actor_id => Some(summary),
                                _ => None,
                            })
                        {
                            note.clone_from(summary);
                        }
                        ActivationResult::Ended {
                            reason,
                            status,
                            note,
                        }
                    }
                })
                .map_err(Clone::clone);
            (activation, activation_result)
        });
        drop(slot.route);
        let result =
            result.map(|finalization| team.engine.publish_resident_finalization(finalization));
        if let Some((activation, activation_result)) = activation {
            let _ = activation.done.send(activation_result);
        }
        slot.done.send_replace(Some(result));
    }
}

/// A lost or ending sidecar cannot hold the episode beyond its stop deadline.
const SIDECAR_STOP_DEADLINE: Duration = Duration::from_secs(5);

async fn stop_sidecar(sidecar: &mut Option<Box<dyn SidecarHandle>>) -> Option<String> {
    let mut handle = sidecar.take()?;
    match tokio::time::timeout(SIDECAR_STOP_DEADLINE, handle.terminate()).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("deadline exceeded".into()),
    }
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
    // Mail the current resident wake injected into this turn was read there.
    let read_through = entry.resident_work.map_or(entry.resident_cursor, |work| {
        entry.resident_cursor.max(work.inbox_through)
    });
    if projection.team.inboxes.get(canonical).is_some_and(|inbox| {
        inbox
            .iter()
            .skip(usize::try_from(read_through).unwrap_or(usize::MAX))
            .any(|mail| mail.from != canonical)
    }) {
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
                .filter(|mail| mail.from != canonical)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut per_channel: BTreeMap<String, usize> = BTreeMap::new();
    let mut notices = 0usize;
    for message in &unread {
        let channel = match &message.to {
            MailEndpoint::Channel(channel) => Some(channel.clone()),
            MailEndpoint::Handle(_) => None,
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
    ) -> Result<hya_store::ResidentArchiveOutcome, CoreError> {
        let _write = self.claim_writes.lock().await;
        if self.team_with_resident(archive.actor).is_some() {
            return Err(CoreError::Invalid(
                "a live episode owns this archive; request its ending".into(),
            ));
        }
        let (outcome, envelopes) = self
            .engine
            .store()
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                root,
                handle,
                archive,
            )
            .await?;
        self.engine.publish_resident_finalization(envelopes);
        Ok(outcome)
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
            root_drivers: Mutex::new(HashMap::new()),
            episodes: Mutex::new(HashMap::new()),
            claim_writes: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            registration_pause: Mutex::new(None),
            #[cfg(test)]
            ending_pause: Mutex::new(None),
            #[cfg(test)]
            activation_lock_pause: Mutex::new(None),
            #[cfg(test)]
            revive_ready_pause: Mutex::new(None),
            #[cfg(test)]
            mail_pause: Mutex::new(None),
            self_ref: self_ref.clone(),
            tasks: TaskTracker::new(),
            listener_stop: CancellationToken::new(),
            listener: Mutex::new(None),
            wait_stalls: Mutex::new(HashMap::new()),
        });
        // The supervisor is the engine's revive seam (ADR-0015): wire it before
        // the listener task starts so no mail send can race the installation.
        supervisor.engine.set_reviver(Arc::downgrade(&supervisor));
        // Turn boundaries deliver wakes deferred by the single-active-turn
        // invariant (mail/quiescence owed to a session whose turn was running).
        let observer: Arc<dyn TurnBoundaryObserver> = supervisor.clone();
        supervisor
            .engine
            .set_turn_observer(Arc::downgrade(&observer));
        let listener = Arc::downgrade(&supervisor);
        let stop = supervisor.listener_stop.clone();
        *supervisor
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(tokio::spawn(Self::run_bus(listener, stop, rx)));
        supervisor
    }

    fn teams(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Arc<TeamActor>>> {
        match self.teams.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The bus listener only routes; each team processes its FIFO independently.
    /// Lag recovery enters those same queues, behind already routed envelopes.
    async fn run_bus(
        supervisor: Weak<Self>,
        stop: CancellationToken,
        mut rx: tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
    ) {
        loop {
            let received = tokio::select! {
                biased;
                () = stop.cancelled() => break,
                received = rx.recv() => received,
            };
            let Some(supervisor) = supervisor.upgrade() else {
                break;
            };
            match received {
                Ok(envelope) => supervisor.route_envelope(envelope),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    for team in supervisor.teams().values() {
                        team.enqueue(TeamEvent::Recover);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    fn route_envelope(&self, envelope: hya_proto::Envelope) {
        if let Event::MailSent { session: root, .. } = &envelope.event {
            // Select and enqueue under the map lock, excluding retirement.
            if let Some(team) = self.teams().get(root) {
                team.enqueue(TeamEvent::Envelope(Box::new(envelope)));
            }
        } else if !matches!(envelope.event, Event::AgentHeartbeat { .. })
            && let Some(actor) = envelope.event.session()
            && let Some(team) = self.team_with_resident(actor)
        {
            team.enqueue(TeamEvent::Envelope(Box::new(envelope)));
        }
    }

    /// Look up a member by episode identity (including killed generations),
    /// or a lead by its current team's root. Neither lookup scans teams.
    fn team_with_resident(&self, session: SessionId) -> Option<Arc<TeamActor>> {
        self.episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .cloned()
            .or_else(|| self.teams().get(&session).cloned())
    }

    /// Reserve under the map lock so retirement cannot cancel a team between
    /// selection and episode installation. Explicit kills still end registrations.
    async fn team_for(&self, root: SessionId) -> Result<TeamRegistration, CoreError> {
        if let Some(team) = self.teams().get(&root).cloned() {
            return Ok(TeamRegistration::new(team));
        }
        let main = self.resolve_main_activation(root).await?;
        let mut teams = self.teams();
        let team = teams
            .entry(root)
            .or_insert_with(|| self.new_team(root, main));
        Ok(TeamRegistration::new(team.clone()))
    }

    /// Hold sole ownership of root continuations until the returned guard drops.
    pub async fn root_driver_activity(
        &self,
        session: SessionId,
    ) -> Result<RootDriverActivity, CoreError> {
        let (root, _) = self.engine.session_lineage(session).await?;
        let team = self.team_for(root).await?;
        {
            let state = team.lock();
            if state.root_driver.runs.load(Ordering::Acquire) == 0 && !self.engine.turn_active(root)
            {
                state.root_driver.autonomous.store(false, Ordering::Release);
            }
            state.root_driver.runs.fetch_add(1, Ordering::AcqRel);
        }
        Ok(RootDriverActivity(team))
    }

    fn new_team(&self, root: SessionId, main: MainActivation) -> Arc<TeamActor> {
        let root_driver = {
            let mut drivers = self
                .root_drivers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = drivers.entry(root).or_default();
            if let Some(active) = entry.upgrade() {
                active
            } else {
                let active = Arc::new(RootDriverState {
                    runs: AtomicUsize::new(0),
                    autonomous: AtomicBool::new(true),
                });
                *entry = Arc::downgrade(&active);
                active
            }
        };
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let team = Arc::new(TeamActor::new(
            root,
            self.engine.clone(),
            self.self_ref.clone(),
            root_driver,
            self.tasks.clone(),
            events,
        ));
        self.tasks.spawn(team.clone().run_events(receiver));
        team.install_main(main);
        team
    }

    fn team_for_with_main(&self, root: SessionId, main: MainActivation) {
        let mut teams = self.teams();
        let team = teams
            .entry(root)
            .or_insert_with(|| self.new_team(root, main));
        let state = team.lock();
        if state.root_driver.runs.load(Ordering::Acquire) == 0 {
            state.root_driver.autonomous.store(true, Ordering::Release);
        }
    }

    fn request_team_end(&self, team: &TeamActor, ending: Ending) -> (bool, Vec<EpisodeDone>) {
        let mut teams = self.teams();
        if teams
            .get(&team.root)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), team))
        {
            teams.remove(&team.root);
        }
        team.request_end(ending)
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
            || state.registrations != 0
            || !state.routing_mail.is_empty()
            || state.queued_wakes != 0
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
            slot.request_end(
                &self.engine,
                *session,
                Ending::Shutdown(FinishCause::Shutdown),
            );
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
        let (_, envelopes) = self
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
            )
            .await?;
        self.engine.publish_resident_finalization(envelopes);
        Ok(())
    }

    async fn resolve_main_activation(&self, root: SessionId) -> Result<MainActivation, CoreError> {
        let mut projection = self.engine.read_projection(root).await?;
        let session = &projection.session;
        let invalid =
            || CoreError::Invalid(format!("root session {root} lacks activation context"));
        let mut base = AgentSpec {
            name: session.agent.clone().ok_or_else(invalid)?,
            model: session.model.clone().ok_or_else(invalid)?,
            workdir: session.workdir.clone().ok_or_else(invalid)?.into(),
            system_prompt: self.engine.base_system_prompt().into(),
            reasoning: None,
        };
        let binding = self
            .engine
            .bind_session_runtime(root, &base.workdir)
            .await?;
        if self
            .engine
            .fall_back_from_missing_agent(root, &projection, &binding)
            .await?
        {
            projection = self.engine.read_projection(root).await?;
            base.name = projection.session.agent.clone().ok_or_else(invalid)?;
        }
        let agent = self
            .engine
            .agent_spec_for_binding(&binding, &base, base.name.as_str())?;
        let agents = self
            .engine
            .agent_roster_for_binding(&binding, base.name.as_str())?;
        let resources = self
            .engine
            .agent_resource_policy_for_binding(&binding, base.name.as_str())?;
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
                done.extend(team.kill(Ending::Shutdown(cause)).await);
            }
            for root in &roots {
                for team in self.episode_teams(*root) {
                    done.extend(team.kill(Ending::Shutdown(cause)).await);
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
                    .end_orphan(root, &handle, actor, &Ending::Shutdown(cause))
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
        self.tasks.close();
        self.tasks.wait().await;
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

    pub(crate) fn forget_wait_stalls(&self, session: SessionId) {
        self.wait_stalls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(caller, member), _| *caller != session && *member != session);
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
        // Serialize insertion with index removal: a late wait snapshot cannot
        // recreate a stall entry after that episode's ending cleaned it up.
        let episodes = self
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if episodes.contains_key(&member) {
            self.wait_stalls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((caller, member), last_answer);
        }
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
        let prefix = format!("{handle}{}", scope::PATH_SEPARATOR);
        // Capture completion handles before any await: deletion may remove both
        // the roster and the episodes while we discover durable descendants.
        let (live, mut captured) = {
            let episodes = self
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut live = None;
            let mut captured = HashMap::new();
            for (session, team) in episodes.iter().filter(|(_, team)| team.root == root) {
                let mut state = team.lock();
                if let Some(slot) = state.residents.get_mut(session)
                    && (slot.handle == handle || slot.handle.starts_with(&prefix))
                {
                    // Suppress later wakes without letting the parent archive
                    // before its descendants. Its claim fences in-flight spawns.
                    slot.user_stopped = true;
                    self.engine.cancel_turn(*session, FinishCause::Archived);
                    if slot.handle == handle {
                        live = Some(*session);
                    }
                    captured.insert(slot.handle.clone(), (*session, slot.done.subscribe()));
                }
            }
            (live, captured)
        };
        let projection = self.engine.read_projection(root).await?;
        let child = live.or_else(|| {
            projection
                .team
                .roster
                .get(handle)
                .map(|entry| entry.session)
        });
        let mut descendants: BTreeMap<_, _> = projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect();
        descendants.extend(
            captured
                .iter()
                .filter(|(path, _)| path.starts_with(&prefix))
                .map(|(path, (session, _))| (path.clone(), *session)),
        );
        let mut descendants: Vec<_> = descendants.into_iter().collect();
        descendants.sort_by_key(|(path, _)| {
            std::cmp::Reverse(path.matches(scope::PATH_SEPARATOR).count())
        });
        #[cfg(test)]
        {
            let pause = self
                .ending_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        for (path, session) in descendants {
            self.stop_and_archive_live(
                root,
                &path,
                session,
                "Workflow run cancelled",
                captured.remove(&path).map(|(_, done)| done),
            )
            .await?;
        }
        if let Some(child) = child {
            self.stop_and_archive_live(
                root,
                handle,
                child,
                "Workflow run cancelled",
                captured.remove(handle).map(|(_, done)| done),
            )
            .await?;
        } else if !projection.team.archived.contains_key(handle) {
            return Err(CoreError::Invalid(format!("unknown resident `{handle}`")));
        }
        // Parent claim release fences new direct children. Sweep anything that
        // committed before release, including descendants of those late children.
        loop {
            let projection = self.engine.read_projection(root).await?;
            let mut descendants: Vec<_> = projection
                .team
                .roster
                .iter()
                .filter(|(path, _)| path.starts_with(&prefix))
                .map(|(path, entry)| (path.clone(), entry.session))
                .collect();
            if descendants.is_empty() {
                break;
            }
            descendants.sort_by_key(|(path, _)| {
                std::cmp::Reverse(path.matches(scope::PATH_SEPARATOR).count())
            });
            for (path, session) in descendants {
                self.stop_and_archive_live(root, &path, session, "Workflow run cancelled", None)
                    .await?;
            }
        }
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
            self.stop_and_archive_live(root, handle, *session, &note, None)
                .await?;
        }
        let cancelled_turn = self
            .stop_and_archive_live(root, &resolved.handle, resolved.session, &note, None)
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
        captured: Option<EpisodeDone>,
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
        if let Some(done) = captured {
            drop(write);
            return await_episode(done).await.map(|()| false);
        }
        let handoff = self.engine.degraded_terminal_handoff(child).await;
        let (_, envelopes) = self
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
            )
            .await?;
        self.engine.publish_resident_finalization(envelopes);
        if let Some(team) = self.teams().get(&root).cloned() {
            team.wake_activated_parent(handle);
        }
        Ok(false)
    }

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
    async fn revive_episode_with_activation(
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
        let projection = self.engine.read_projection(root).await?;
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
        let (agent, (binding, agents, resources, sidecar_factory)) =
            if let Some(activation) = &activation {
                activation.resolved.clone()
            } else {
                let child_projection = self.engine.read_projection(child).await?;
                let stable =
                    child_projection.session.agent.as_ref().ok_or_else(|| {
                        CoreError::Invalid("resident has no recorded agent".into())
                    })?;
                let workdir = child_projection
                    .session
                    .workdir
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| CoreError::Invalid(format!("session not found: {child}")))?;
                let binding = self.engine.bind_session_runtime(child, &workdir).await?;
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
                self.engine
                    .resolve_resident_runtime(binding, &base, stable)?
            };
        let channel_policy =
            ChannelPolicy::from_binding(&binding)?.snapshot_for(agent.name.as_str());
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
        let current = loop {
            let current = self.engine.read_projection(root).await?;
            // A live roster wins: never wait for a concurrent revival's episode.
            if current.team.roster.contains_key(target) {
                break current;
            }
            let previous = self.team_with_resident(child).and_then(|previous| {
                previous
                    .lock()
                    .residents
                    .get(&child)
                    .map(|slot| slot.done.subscribe())
            });
            let Some(done) = previous else {
                break current;
            };
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
            } else if current.team.roster.contains_key(target) {
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
            let projection = self.engine.read_projection(root).await?;
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
        let parent_session = projection
            .team
            .roster
            .get(parent)
            .map_or(root, |entry| entry.session);
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
            MainActivation {
                agent,
                binding,
                agents,
                resources,
                guidance,
            },
        );
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
                    directive.clone(),
                    Some(directive),
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
                    directive.clone(),
                    Some(directive),
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
    /// payload or input values. Returns the committed Session, handle and member
    /// id before initializer hooks complete; the episode runs those before work.
    pub async fn spawn_resident_parked(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        registration_directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        let (root, prepared, member) = self
            .prepare_resident(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(registration_directive, None, parent_claim, guidance),
            )
            .await?;
        let session = prepared.session;
        let handle = scope::join_path(&prepared.parent_path, &prepared.leaf);
        // Return the committed identity before initialization can wait on a
        // plugin. Activation and cancellation then belong to the controller.
        self.register_episodes(root, vec![prepared], None).await?;
        Ok((session, handle, member))
    }

    async fn spawn_resident_inner(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        context: ResidentSpawnContext,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        let (root, prepared, member) = self
            .prepare_resident(parent, agent, resolved, context)
            .await?;
        let session = prepared.session;
        let handle = scope::join_path(&prepared.parent_path, &prepared.leaf);
        let initialized = self.register_episodes(root, vec![prepared], None).await?;
        let _ = futures::future::join_all(initialized).await;
        Ok((session, handle, member))
    }

    /// Register all resolved task members or none, then install every committed
    /// episode under the claim-write boundary before awaiting initialization.
    pub async fn spawn_residents_typed(
        &self,
        parent: SessionId,
        members: Vec<TaskResidentSpawn>,
        main: MainActivation,
        parent_claim: Option<&ActorClaim>,
    ) -> Result<Vec<(SessionId, String)>, CoreError> {
        let (root, _) = self.engine.session_lineage(parent).await?;
        let mut prepared = Vec::with_capacity(members.len());
        let mut identities = Vec::with_capacity(members.len());
        for member in members {
            let (_, registration, _) = self
                .prepare_resident(
                    parent,
                    member.agent,
                    member.runtime,
                    ResidentSpawnContext::new(
                        member.directive.clone(),
                        Some(member.directive),
                        parent_claim,
                        member.guidance,
                    )
                    .with_task(member.task),
                )
                .await?;
            identities.push((
                registration.session,
                scope::join_path(&registration.parent_path, &registration.leaf),
            ));
            prepared.push(registration);
        }
        let initialized = self.register_episodes(root, prepared, Some(main)).await?;
        let _ = futures::future::join_all(initialized).await;
        Ok(identities)
    }

    async fn prepare_resident(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        context: ResidentSpawnContext,
    ) -> Result<(SessionId, PreparedResident, MemberId), CoreError> {
        let ResidentSpawnContext {
            directive,
            initial,
            parent_claim,
            guidance,
            task,
        } = context;
        let prefix = hya_tool::sanitize_handle_prefix(
            task.as_ref()
                .map_or(agent.name.as_str(), |task| task.subagent_type.as_str()),
        );
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
        if parent_depth.saturating_add(1) > crate::MAX_SUBAGENT_DEPTH {
            return Err(CoreError::Invalid("maximum subagent depth exceeded".into()));
        }
        let parent_path = if parent == root {
            scope::ROOT_HANDLE.into()
        } else {
            self.engine.resolve_handle(root, parent).await?
        };
        let reservation = {
            // A commit may release a reservation only after this snapshot has
            // observed its durable identity; stale snapshots cannot reuse it.
            let _write = self.claim_writes.lock().await;
            self.engine.mint_member_leaf(root, &prefix).await
        };
        let leaf = reservation.leaf.clone();
        let member = MemberId::new();
        let description = task
            .as_ref()
            .map(|task| task.description.trim())
            .filter(|description| !description.is_empty())
            .map_or_else(|| directive.chars().take(80).collect(), ToString::to_string);
        let tool_call = task.as_ref().and_then(|task| task.tool_call);
        events.push(Event::MemberSpawned {
            session: parent,
            member,
            child: Some(session),
            subagent_type: agent.name.clone(),
            description,
            depth: parent_depth.saturating_add(1),
            directive,
            tool_call,
        });
        Ok((
            root,
            PreparedResident {
                reservation: Some(reservation),
                session,
                parent_path,
                leaf,
                agent,
                runtime: ResidentRuntimeContext::Resolved {
                    binding,
                    agents,
                    resources,
                },
                activation: ResidentActivation {
                    events,
                    parent_claim,
                    initial,
                    guidance,
                    route: None,
                    sidecar_factory,
                },
            },
            member,
        ))
    }

    /// Arm an idle member (or revive an archived one) with durable parent mail.
    /// Guidance, route and the post-mail inbox boundary are installed before
    /// the turn reservation is released. The episode alone resolves completion.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn activate(
        &self,
        parent: SessionId,
        member: SessionId,
        resolved: (AgentSpec, ResolvedResidentRuntime),
        directive: String,
        guidance: Arc<str>,
        route: Option<WorkflowTurnRoute>,
        parent_claim: Option<&ActorClaim>,
    ) -> Result<ActivationWaiter, CoreError> {
        if directive.trim().is_empty() {
            return Err(CoreError::Invalid(
                "Workflow activation directive must not be blank".into(),
            ));
        }
        let (root, _) = self.engine.session_lineage(parent).await?;
        let from = self.engine.resolve_handle(root, parent).await?;
        #[cfg(test)]
        {
            let pause = self
                .activation_lock_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let write = self.claim_writes.lock().await;
        let projection = self.engine.read_projection(root).await?;
        let target = projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == member)
            .map(|entry| entry.handle.clone())
            .or_else(|| {
                projection
                    .team
                    .archived
                    .values()
                    .find(|entry| entry.session == member)
                    .map(|entry| entry.handle.clone())
            })
            .ok_or_else(|| CoreError::Invalid("resident member is not registered".into()))?;
        let dm = crate::engine::dm_channel_between(&projection, &from, &target)
            .ok_or_else(|| CoreError::Invalid("resident member has no parent DM channel".into()))?;
        let (done, receiver) = oneshot::channel();
        if projection.team.roster.contains_key(&target) {
            let lease = self.engine.try_begin_turn(member)?;
            let team = self
                .team_with_resident(member)
                .ok_or_else(|| CoreError::Invalid("resident session is not registered".into()))?;
            {
                let state = team.lock();
                let slot = &state.residents[&member];
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
            }
            let envelope = self
                .engine
                .store()
                .append_channel_mail(
                    root,
                    from,
                    dm.clone(),
                    hya_proto::MailKind::Message,
                    directive,
                    parent_claim,
                    None,
                )
                .await?
                .0;
            let boundary = self.engine.read_projection(root).await?.team.inboxes[&target].len();
            {
                let mut state = team.lock();
                let slot = state.residents.get_mut(&member).ok_or_else(|| {
                    CoreError::Invalid("resident episode disappeared during activation".into())
                })?;
                slot.guidance = Some(guidance);
                slot.route = route;
                slot.activation = Some(ActivationCompletion { boundary, done });
                slot.pending = true;
            }
            self.engine.publish_envelope(envelope);
            drop(write);
            drop(lease);
            return Ok(ActivationWaiter { done: receiver });
        }
        drop(write);
        self.revive_episode_with_activation(
            root,
            &from,
            &target,
            dm,
            hya_proto::MailKind::Message,
            directive,
            parent_claim,
            None,
            Some(WorkflowActivation {
                resolved,
                guidance,
                route,
                done,
            }),
        )
        .await?;
        Ok(ActivationWaiter { done: receiver })
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
        let initialized = self
            .register_episodes(
                root,
                vec![PreparedResident {
                    reservation: None,
                    session,
                    parent_path,
                    leaf,
                    agent,
                    runtime,
                    activation,
                }],
                None,
            )
            .await?;
        let _ = futures::future::join_all(initialized).await;
        Ok(())
    }

    fn capacity(&self) -> u64 {
        self.engine
            .governor()
            .map_or(u64::MAX, |governor| governor.limits().per_run_budget)
    }

    async fn register_episodes(
        &self,
        root: SessionId,
        members: Vec<PreparedResident>,
        main: Option<MainActivation>,
    ) -> Result<Vec<oneshot::Receiver<Result<(), CoreError>>>, CoreError> {
        let projection = self.engine.read_projection(root).await?;
        let parent_claim = members
            .first()
            .and_then(|member| member.activation.parent_claim);
        let mut registrations = Vec::with_capacity(members.len());
        let mut installs = Vec::with_capacity(members.len());
        let mut reservations = Vec::with_capacity(members.len());
        for member in members {
            let PreparedResident {
                reservation,
                session,
                parent_path,
                leaf,
                agent,
                runtime,
                activation,
            } = member;
            reservations.push(reservation);
            let ResidentActivation {
                mut events,
                parent_claim: _,
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
                    channel: dm_channel.clone(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec![parent_path.clone(), handle.clone()],
                },
            ]);
            let initial = initial.filter(|text| !text.trim().is_empty());
            let pending = initial.is_some();
            if let Some(text) = initial {
                events.push(Event::MailSent {
                    session: root,
                    from: parent_path.clone(),
                    to: MailEndpoint::Channel(dm_channel),
                    kind: hya_proto::MailKind::Message,
                    body: text,
                });
            }
            let channel_policy =
                ChannelPolicy::from_binding(&binding)?.snapshot_for(agent.name.as_str());

            let parent_session = projection
                .team
                .roster
                .get(&parent_path)
                .map_or(root, |entry| entry.session);
            registrations.push(hya_store::EpisodeRegistration {
                actor: session,
                events,
            });
            let episode = Episode {
                handle,
                agent,
                binding,
                agents,
                resources,
                guidance,
                route,
                activation: None,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: None,
                cancel: CancellationToken::new(),
                ending: None,
                done: watch::channel(None).0,
                cursor: 0,
                last_activity: None,
                last_heartbeat_emit: None,
                notify: Arc::new(Notify::new()),
            };
            installs.push((session, parent_session, episode, channel_policy));
        }
        let mut team = if let Some(main) = main {
            let mut teams = self.teams();
            let team = teams
                .entry(root)
                .or_insert_with(|| self.new_team(root, main));
            {
                let state = team.lock();
                if state.root_driver.runs.load(Ordering::Acquire) == 0 {
                    state.root_driver.autonomous.store(true, Ordering::Release);
                }
            }
            TeamRegistration::new(team.clone())
        } else {
            self.team_for(root).await?
        };
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
        loop {
            let previous = registrations.iter().find_map(|registration| {
                self.team_with_resident(registration.actor)
                    .and_then(|team| {
                        team.lock()
                            .residents
                            .get(&registration.actor)
                            .map(|slot| (registration.actor, slot.done.subscribe()))
                    })
            });
            let Some((actor, done)) = previous else { break };
            if !projection
                .team
                .archived
                .values()
                .any(|entry| entry.session == actor)
            {
                return Err(CoreError::Invalid(
                    "resident already has a live episode".into(),
                ));
            }
            drop(write);
            let _ = await_episode(done).await;
            team = self.team_for(root).await?;
            write = self.claim_writes.lock().await;
        }
        let registered = self
            .engine
            .store()
            .register_episodes(
                root,
                self.owner_run_id,
                parent_claim.as_ref(),
                self.capacity(),
                registrations,
            )
            .await?;
        // Committed identities now belong to durable history. Release while
        // claim_writes still excludes name allocation from a stale snapshot.
        drop(reservations);
        let mut initialized_members = Vec::with_capacity(installs.len());
        let mut publications = Vec::with_capacity(installs.len());
        for ((claim, envelopes), (session, parent, mut episode, channel_policy)) in
            registered.into_iter().zip(installs)
        {
            self.engine
                .update_session_channel_policy(session, channel_policy);
            let (ready, initialized) = oneshot::channel();
            let (publication, published) = oneshot::channel();
            let (child_envelopes, parent_envelopes): (Vec<_>, Vec<_>) = envelopes
                .into_iter()
                .partition(|envelope| envelope.event.session() == Some(session));
            let init = EpisodeInit {
                binding: episode.binding.clone(),
                agent: episode.agent.name.clone(),
                parent,
                envelopes: child_envelopes,
                published,
                ready,
            };
            episode.claim = Some(claim);
            episode.cancel = team.cancel.child_token();
            team.install_episode(session, episode, Some(init));
            initialized_members.push(initialized);
            publications.push((parent_envelopes, publication));
        }
        for (envelopes, _) in &publications {
            for envelope in envelopes {
                self.engine.publish_envelope(envelope.clone());
            }
        }
        for (_, publication) in publications {
            let _ = publication.send(());
        }
        drop(write);
        // Commit is the batch's success boundary. Each episode owns initializer
        // errors and its ending; none can retract these committed identities.
        Ok(initialized_members)
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

#[cfg(test)]
mod tests;
