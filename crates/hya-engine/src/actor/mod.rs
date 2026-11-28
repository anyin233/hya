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

use hya_proto::{
    ActorClaim, Event, FinishCause, FinishReason, MailEndpoint, MemberRunStatus, OwnerRunId,
    RosterStatus, SessionId, ToolCallId, scope,
};
use hya_tool::extension::ExtensionEvent;
use hya_tool::{AgentDef, ArchiveReason, ArchiveReceipt, ReportOutcome, ResolvedTool};
use tokio::sync::{Notify, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::engine::{
    AgentSpec, ArchiveReviver, DRAIN_DEADLINE, ExplicitTurnRoute, SessionEngine,
    TurnBoundaryObserver, TurnDrainReport, TurnLease, TurnRequestContext,
};
use crate::error::CoreError;
use crate::hooks::{HookChain, NativeAgentSpawnInput, scope_activation_hooks};
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
    route: Option<ExplicitTurnRoute>,
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
    claim: crate::store_bridge::RecoveredActorClaim,
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
mod episode_settlement;
mod inbox;
mod recovery;
mod report;
mod spawn;
mod supervisor;
pub use recovery::{ResidentRecoveryError, recover_resident_claims};
pub use spawn::{
    ResolveSpawnMemberCtx, ResolvedSpawnMember, resolve_main_activation_context,
    resolve_spawn_member, resolve_spawn_member_with_catalog,
};
mod team;
mod team_mail;

use ending::finish_episode;
pub(crate) use ending::resolve_member_target;
use ending::stop_sidecar;
use ending::{Ending, commit_episode_ending};
use episode::await_episode;
use episode::episode_task;
use episode::{
    Action, ActivationCompletion, Episode, EpisodeDone, EpisodeInit, RunPlan, SlotStatus,
    WorkflowActivation,
};
pub use episode::{ActivationResult, ActivationWaiter};
use report::PendingReport;
use team::{TeamActor, TeamEvent, TeamPhase};
