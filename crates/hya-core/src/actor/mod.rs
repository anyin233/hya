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

/// Injected when the team quiesces so the main agent synthesizes autonomously.
const SYNTHESIS_DIRECTIVE: &str = "TEAM QUIESCED — every team member is idle and no mail is in flight. \
Review the team's results (roster, channels, and your inbox) and produce the final synthesized answer. \
If more work is genuinely required, delegate it; otherwise conclude.";

/// Minimum spacing between harness heartbeats for one busy slot (ADR-0002
/// liveness). A busy child that is really progressing emits fresh engine
/// events; the supervisor folds those observations into `AgentHeartbeat` at
/// most this often so the parent can tell progress from a stall.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

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

#[cfg_attr(test, derive(Debug))]
enum TeamEvent {
    #[cfg(test)]
    Flush(oneshot::Sender<()>),
    Mail {
        seq: hya_proto::EventSeq,
        from: String,
        to: MailEndpoint,
    },
    Activity(SessionId),
    Recover,
}

impl TeamEvent {
    fn keeps_team_live(&self) -> bool {
        match self {
            Self::Mail { .. } => true,
            Self::Activity(_) => false,
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
    recovered: Mutex<Vec<RecoveredResident>>,
    tasks: TaskTracker,
    listener_stop: CancellationToken,
    listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// `wait` stall notices already given: (waiting session, member session)
    /// → the member's last assistant message when it was reported stopped.
    /// In-memory only: after a restart a stopped member is reported once more.
    wait_stalls: Mutex<HashMap<(SessionId, SessionId), WaitStall>>,
}

struct WaitStall {
    claim: ActorClaim,
    last_answer: Option<hya_proto::MessageId>,
}

struct RecoveredResident {
    root: SessionId,
    handle: String,
    agent: AgentSpec,
    runtime: ResolvedResidentRuntime,
    claim: hya_store::RecoveredActorClaim,
    disposition: ResidentRecovery,
}

type EpisodeFinalization = Vec<hya_proto::Envelope>;

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

#[cfg(test)]
mod tests;

mod ending;
mod episode;
mod supervisor;
mod team;

use ending::commit_episode_ending;
use ending::finish_episode;
pub(crate) use ending::resolve_member_target;
use ending::stop_sidecar;
use episode::await_episode;
use episode::episode_task;
use team::recipient_channel_role;
use team::resident_delivery_allowed;
