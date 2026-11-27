//! Domain extension events, payloads, and projection reducers (Phase 0 v2 bridge).
//!
//! In `hya` v2, domain-specific events (compaction observability, workflow,
//! agent-channels, multi-agent) are persisted as [`Event::Extension`] and folded
//! via [`ProjectionReducer`]s into [`Projection::extensions`] (and
//! [`PartProjection::Tool::eviction_notice`] for compaction eviction).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hya_proto::tokens::{TokenAccountingMode, TokenSource};
use hya_proto::{
    ActorEpoch, AgentName, ChannelKind, Event, MailEndpoint, MailKind, MemberId, MemberRunStatus,
    ModelRef, OwnerRunId, PartId, PartProjection, Projection, ProjectionReducer,
    ProjectionReducerRegistry, RosterStatus, SessionId, SubagentMode, ToolCallId, WorkflowIdentity,
    WorkflowRunId, scope,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lifecycle::{ArchiveReason, ReportOutcome};
use crate::workflow_plane::{
    WorkflowMemberProjection, WorkflowMemberRole, WorkflowProjection, WorkflowRouteFailureClass,
    WorkflowRunProjection, WorkflowRunStatus, WorkflowStagePlan, WorkflowStageProjection,
    WorkflowStageRouteOutcome, WorkflowStageStatus,
};

/// Bundle id owning compaction extension events and projection state.
pub const BUNDLE_COMPACTION: &str = "hya/compaction";
/// Bundle id owning workflow extension events and projection state.
pub const BUNDLE_WORKFLOW: &str = "hya/workflow";
/// Bundle id owning channel/mailbox extension events and projection state.
pub const BUNDLE_CHANNELS: &str = "hya/agent-channels";
/// Bundle id owning multi-agent roster/member/handoff extension events and state.
pub const BUNDLE_MULTI_AGENT: &str = "hya/multi-agent";

/// One tool output replaced during compaction eviction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvictedPart {
    /// Evicted tool part id.
    pub part: PartId,
    /// Replacement notice text sent in place of the output.
    pub notice: String,
}

/// Latest window-occupancy report, mirroring the newest `context_status` event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextStatusProjection {
    /// Tokens the last measured request is believed to occupy.
    pub tokens: u64,
    /// Whether the count was provider-anchored or locally estimated.
    pub source: TokenSource,
    /// Accounting mode in force for that round.
    pub mode: TokenAccountingMode,
    /// Resolved compaction threshold the count was judged against.
    pub threshold: u64,
}

/// Latest terminal handoff folded onto a child session (ADR-0015).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HandoffProjection {
    /// Episode generation of this document.
    pub generation: u32,
    /// Full six-section state document.
    pub doc: String,
    /// A deterministic degraded fallback produced this document.
    #[serde(default)]
    pub degraded: bool,
}

/// A single spawned subagent as seen from its parent session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemberProjection {
    /// Member id on the parent log.
    pub member: MemberId,
    /// Child session when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<SessionId>,
    /// Subagent type / agent name for the spawn.
    pub subagent_type: AgentName,
    /// Short task description from spawn.
    pub description: String,
    /// Depth in the subagent tree.
    pub depth: u32,
    /// Latest lifecycle status.
    pub status: MemberRunStatus,
    /// Bounded finish summary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Verbatim parent directive that defines this member's purpose.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub directive: String,
    /// Tool call that caused this spawn, when it came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCallId>,
}

/// Outstanding resident wake, retained across a user-cancelled Idle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidentWorkProjection {
    /// Actor epoch that started this work.
    pub epoch: ActorEpoch,
    /// Inbox length delivered to this wake; settling it acknowledges this boundary.
    pub inbox_through: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// A live team member: its handle, own session, and declared agent type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RosterEntry {
    /// Stable canonical path, e.g. `main/lead-1/worker-2`.
    pub handle: String,
    /// Agent's own session id.
    pub session: SessionId,
    /// Declared agent type for display.
    #[serde(default)]
    pub agent_type: AgentName,
    /// Transient vs resident scheduling.
    #[serde(default)]
    pub mode: SubagentMode,
    /// Live activity status.
    #[serde(default)]
    pub status: RosterStatus,
    /// Short human-facing description of current task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    /// Inbox offset acknowledged within this episode.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resident_cursor: u64,
    /// Latest harness-observed activity time (Unix-epoch ms).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub heartbeat_ms: u64,
    /// Present only after work starts and before it reaches a terminal/idle state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_work: Option<ResidentWorkProjection>,
}

impl RosterEntry {
    /// Whether the replayed member state accepts new mail.
    #[must_use]
    pub fn accepts_mail(&self) -> bool {
        !self.mode.is_resident()
            || !matches!(self.status, RosterStatus::Done | RosterStatus::Failed)
    }
}

/// One archived agent's history row (ADR-0015), folded from `agent_archived`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArchivedEntry {
    /// Canonical handle the agent had while live.
    pub handle: String,
    /// Agent's own session id.
    pub session: SessionId,
    /// Declared agent type.
    #[serde(default)]
    pub agent_type: AgentName,
    /// Why the agent archived.
    pub reason: ArchiveReason,
}

/// One channel: its current subscribers plus the ordered log of everything posted to it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelProjection {
    /// Group broadcast vs DM pair.
    #[serde(default)]
    pub kind: ChannelKind,
    /// Owning unit path for group channels; `None` for DM pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Current subscriber handles (no leading `#`).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub members: BTreeSet<String>,
    /// Indices into `TeamProjection::mail`, in channel delivery order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log: Vec<usize>,
}

/// A single delivered message as folded into an inbox / channel log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MailMessage {
    /// Sender handle.
    pub from: String,
    /// Persisted channel target, or a handle for a harness notice.
    pub to: MailEndpoint,
    /// Chatter vs announcement.
    #[serde(default)]
    pub kind: MailKind,
    /// Message body.
    pub body: String,
}

/// Combined team-scoped mailbox/channel + roster/archive state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamProjection {
    /// Mail records in fold order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mail: Vec<MailMessage>,
    /// Per-agent inbox keyed by canonical path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inboxes: BTreeMap<String, Vec<usize>>,
    /// Channels keyed by their minted id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, ChannelProjection>,
    /// Live team roster keyed by canonical path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roster: BTreeMap<String, RosterEntry>,
    /// Archived team history keyed by canonical path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archived: BTreeMap<String, ArchivedEntry>,
}

impl TeamProjection {
    /// Incoming mail past a raw inbox cursor.
    pub fn unread_incoming_mail<'a>(
        &'a self,
        handle: &'a str,
        through: u64,
    ) -> impl Iterator<Item = &'a MailMessage> {
        self.inboxes
            .get(handle)
            .into_iter()
            .flatten()
            .skip(usize::try_from(through).unwrap_or(usize::MAX))
            .map(|index| &self.mail[*index])
            .filter(move |mail| mail.from != handle)
    }

    /// Whether a channel subscriber receives mail in the shared replay model.
    #[must_use]
    pub fn mail_recipient_is_live(&self, handle: &str) -> bool {
        self.roster
            .get(handle)
            .is_none_or(RosterEntry::accepts_mail)
    }

    /// Resolve an input handle (canonical path, leaf, or parent sentinel) to
    /// the caller's existing DM.
    #[must_use]
    pub fn resolve_dm(&self, from: &str, raw: &str) -> Option<String> {
        let raw = raw.trim();
        let peer = match raw {
            "" | "^" | "^parent" => scope::parent_path(from)?,
            other => other,
        };
        let mut matches = self.channels.iter().filter(|(_, channel)| {
            channel.kind == ChannelKind::Dm
                && channel.members.contains(from)
                && channel
                    .members
                    .iter()
                    .any(|member| member != from && (member == peer || scope::leaf(member) == peer))
        });
        match (matches.next(), matches.next()) {
            (Some((id, _)), None) => Some(id.clone()),
            _ => None,
        }
    }
}

/// Extension state folded under `projection.extensions["hya/compaction"]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionExtensionState {
    /// Latest context status occupancy snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_status: Option<ContextStatusProjection>,
}

/// Extension state folded under `projection.extensions["hya/agent-channels"]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelsExtensionState {
    /// Mail records in fold order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mail: Vec<MailMessage>,
    /// Per-agent inbox keyed by canonical path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inboxes: BTreeMap<String, Vec<usize>>,
    /// Channels keyed by their minted id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, ChannelProjection>,
}

/// Extension state folded under `projection.extensions["hya/multi-agent"]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MultiAgentExtensionState {
    /// Spawned members on the parent log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberProjection>,
    /// Latest terminal handoff committed on a child log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffProjection>,
    /// Live team roster keyed by canonical path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roster: BTreeMap<String, RosterEntry>,
    /// Archived team history keyed by canonical path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archived: BTreeMap<String, ArchivedEntry>,
}

/// Typed representation of domain extension events persisted via [`Event::Extension`].
#[allow(missing_docs)]
#[derive(Clone, Debug, PartialEq)]
pub enum ExtensionEvent {
    // ---- hya/compaction ----
    ContextEvicted {
        session: SessionId,
        evicted_parts: u32,
        parts: Vec<EvictedPart>,
        tokens_before: u64,
        tokens_after: u64,
        threshold: u64,
    },
    ContextStatus {
        session: SessionId,
        tokens: u64,
        source: TokenSource,
        mode: TokenAccountingMode,
        threshold: u64,
    },

    // ---- hya/workflow ----
    WorkflowSelected {
        session: SessionId,
        workflow: WorkflowIdentity,
    },
    WorkflowRunStarted {
        session: SessionId,
        run: WorkflowRunId,
        workflow: WorkflowIdentity,
        request_hash: String,
        owner: OwnerRunId,
        stages: Vec<WorkflowStagePlan>,
    },
    WorkflowStageStarted {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
    },
    WorkflowStageMemberLinked {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
        member: MemberId,
        role: WorkflowMemberRole,
        iteration: u32,
    },
    WorkflowStageRouteOutcome {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
        member: MemberId,
        role: WorkflowMemberRole,
        iteration: u32,
        step: u32,
        candidate_index: u32,
        model: ModelRef,
        reasoning: String,
        failure_class: WorkflowRouteFailureClass,
    },
    WorkflowStageFinished {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
        status: WorkflowStageStatus,
    },
    WorkflowRunFinished {
        session: SessionId,
        run: WorkflowRunId,
        status: WorkflowRunStatus,
        error: Option<String>,
    },

    // ---- hya/agent-channels ----
    MailSent {
        session: SessionId,
        from: String,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
    },
    ChannelJoined {
        session: SessionId,
        channel: String,
        member: String,
    },
    ChannelCreated {
        session: SessionId,
        channel: String,
        kind: ChannelKind,
        unit: Option<String>,
        members: Vec<String>,
    },
    MailConsumed {
        session: SessionId,
        handle: String,
        through: u64,
    },

    // ---- hya/multi-agent ----
    SessionStatus {
        session: SessionId,
        status: Value,
    },
    MemberSpawned {
        session: SessionId,
        member: MemberId,
        child: Option<SessionId>,
        subagent_type: AgentName,
        description: String,
        depth: u32,
        directive: String,
        tool_call: Option<ToolCallId>,
    },
    MemberStatusChanged {
        session: SessionId,
        member: MemberId,
        status: MemberRunStatus,
    },
    MemberFinished {
        session: SessionId,
        member: MemberId,
        status: MemberRunStatus,
        summary: String,
        child: Option<SessionId>,
    },
    AgentRegistered {
        session: SessionId,
        agent_session: SessionId,
        handle: String,
        parent: Option<String>,
        agent_type: AgentName,
        mode: SubagentMode,
    },
    AgentActivityChanged {
        session: SessionId,
        handle: String,
        status: RosterStatus,
        current_task: Option<String>,
    },
    ResidentWorkStarted {
        session: SessionId,
        actor_session: SessionId,
        handle: String,
        epoch: ActorEpoch,
        inbox_through: u64,
    },
    ResidentWorkFinished {
        session: SessionId,
        handle: String,
        epoch: ActorEpoch,
        inbox_through: u64,
    },
    AgentHeartbeat {
        session: SessionId,
        handle: String,
        heartbeat_ms: u64,
    },
    SubagentReported {
        session: SessionId,
        member: MemberId,
        child: SessionId,
        handle: String,
        outcome: ReportOutcome,
        report: String,
    },
    HandoffCommitted {
        session: SessionId,
        handle: String,
        generation: u32,
        doc: String,
        degraded: bool,
    },
    AgentArchived {
        session: SessionId,
        handle: String,
        child: SessionId,
        reason: ArchiveReason,
    },
    AgentRestarted {
        session: SessionId,
        handle: String,
        child: SessionId,
        epoch: ActorEpoch,
    },
}

#[derive(Serialize, Deserialize)]
struct ContextEvictedWire {
    #[serde(default)]
    evicted_parts: u32,
    #[serde(default)]
    parts: Vec<EvictedPart>,
    #[serde(default)]
    tokens_before: u64,
    #[serde(default)]
    tokens_after: u64,
    #[serde(default)]
    threshold: u64,
}

#[derive(Serialize, Deserialize)]
struct WorkflowSelectedWire {
    workflow: WorkflowIdentity,
}

#[derive(Serialize, Deserialize)]
struct WorkflowRunStartedWire {
    run: WorkflowRunId,
    workflow: WorkflowIdentity,
    request_hash: String,
    owner: OwnerRunId,
    stages: Vec<WorkflowStagePlan>,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageStartedWire {
    run: WorkflowRunId,
    stage: String,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageMemberLinkedWire {
    run: WorkflowRunId,
    stage: String,
    member: MemberId,
    role: WorkflowMemberRole,
    iteration: u32,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageRouteOutcomeWire {
    run: WorkflowRunId,
    stage: String,
    member: MemberId,
    role: WorkflowMemberRole,
    iteration: u32,
    step: u32,
    candidate_index: u32,
    model: ModelRef,
    reasoning: String,
    failure_class: WorkflowRouteFailureClass,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageFinishedWire {
    run: WorkflowRunId,
    stage: String,
    status: WorkflowStageStatus,
}

#[derive(Serialize, Deserialize)]
struct WorkflowRunFinishedWire {
    run: WorkflowRunId,
    status: WorkflowRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MailSentWire {
    from: String,
    to: MailEndpoint,
    #[serde(default)]
    kind: MailKind,
    body: String,
}

#[derive(Serialize, Deserialize)]
struct ChannelJoinedWire {
    channel: String,
    member: String,
}

#[derive(Serialize, Deserialize)]
struct ChannelCreatedWire {
    channel: String,
    #[serde(default)]
    kind: ChannelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unit: Option<String>,
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct MailConsumedWire {
    handle: String,
    through: u64,
}

#[derive(Serialize, Deserialize)]
struct SessionStatusWire {
    status: Value,
}

#[derive(Serialize, Deserialize)]
struct MemberSpawnedWire {
    member: MemberId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child: Option<SessionId>,
    subagent_type: AgentName,
    description: String,
    depth: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    directive: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call: Option<ToolCallId>,
}

#[derive(Serialize, Deserialize)]
struct MemberStatusChangedWire {
    member: MemberId,
    status: MemberRunStatus,
}

#[derive(Serialize, Deserialize)]
struct MemberFinishedWire {
    member: MemberId,
    status: MemberRunStatus,
    summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child: Option<SessionId>,
}

#[derive(Serialize, Deserialize)]
struct AgentRegisteredWire {
    agent_session: SessionId,
    handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(default)]
    agent_type: AgentName,
    #[serde(default)]
    mode: SubagentMode,
}

#[derive(Serialize, Deserialize)]
struct AgentActivityChangedWire {
    handle: String,
    status: RosterStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_task: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ResidentWorkStartedWire {
    actor_session: SessionId,
    handle: String,
    epoch: ActorEpoch,
    inbox_through: u64,
}

#[derive(Serialize, Deserialize)]
struct ResidentWorkFinishedWire {
    handle: String,
    epoch: ActorEpoch,
    inbox_through: u64,
}

#[derive(Serialize, Deserialize)]
struct AgentHeartbeatWire {
    handle: String,
    heartbeat_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct SubagentReportedWire {
    member: MemberId,
    child: SessionId,
    handle: String,
    outcome: ReportOutcome,
    report: String,
}

#[derive(Serialize, Deserialize)]
struct HandoffCommittedWire {
    handle: String,
    generation: u32,
    doc: String,
    #[serde(default)]
    degraded: bool,
}

#[derive(Serialize, Deserialize)]
struct AgentArchivedWire {
    handle: String,
    child: SessionId,
    reason: ArchiveReason,
}

#[derive(Serialize, Deserialize)]
struct AgentRestartedWire {
    handle: String,
    child: SessionId,
    epoch: ActorEpoch,
}

fn to_value<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

impl ExtensionEvent {
    /// Owning bundle identifier for [`Event::Extension::bundle`].
    #[must_use]
    pub const fn bundle_id(&self) -> &'static str {
        match self {
            Self::ContextEvicted { .. } | Self::ContextStatus { .. } => BUNDLE_COMPACTION,
            Self::WorkflowSelected { .. }
            | Self::WorkflowRunStarted { .. }
            | Self::WorkflowStageStarted { .. }
            | Self::WorkflowStageMemberLinked { .. }
            | Self::WorkflowStageRouteOutcome { .. }
            | Self::WorkflowStageFinished { .. }
            | Self::WorkflowRunFinished { .. } => BUNDLE_WORKFLOW,
            Self::MailSent { .. }
            | Self::ChannelJoined { .. }
            | Self::ChannelCreated { .. }
            | Self::MailConsumed { .. } => BUNDLE_CHANNELS,
            Self::SessionStatus { .. }
            | Self::MemberSpawned { .. }
            | Self::MemberStatusChanged { .. }
            | Self::MemberFinished { .. }
            | Self::AgentRegistered { .. }
            | Self::AgentActivityChanged { .. }
            | Self::ResidentWorkStarted { .. }
            | Self::ResidentWorkFinished { .. }
            | Self::AgentHeartbeat { .. }
            | Self::SubagentReported { .. }
            | Self::HandoffCommitted { .. }
            | Self::AgentArchived { .. }
            | Self::AgentRestarted { .. } => BUNDLE_MULTI_AGENT,
        }
    }

    /// Extension event kind tag for [`Event::Extension::kind`].
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ContextEvicted { .. } => "context_evicted",
            Self::ContextStatus { .. } => "context_status",
            Self::WorkflowSelected { .. } => "workflow_selected",
            Self::WorkflowRunStarted { .. } => "workflow_run_started",
            Self::WorkflowStageStarted { .. } => "workflow_stage_started",
            Self::WorkflowStageMemberLinked { .. } => "workflow_stage_member_linked",
            Self::WorkflowStageRouteOutcome { .. } => "workflow_stage_route_outcome",
            Self::WorkflowStageFinished { .. } => "workflow_stage_finished",
            Self::WorkflowRunFinished { .. } => "workflow_run_finished",
            Self::MailSent { .. } => "mail_sent",
            Self::ChannelJoined { .. } => "channel_joined",
            Self::ChannelCreated { .. } => "channel_created",
            Self::MailConsumed { .. } => "mail_consumed",
            Self::SessionStatus { .. } => "session_status",
            Self::MemberSpawned { .. } => "member_spawned",
            Self::MemberStatusChanged { .. } => "member_status_changed",
            Self::MemberFinished { .. } => "member_finished",
            Self::AgentRegistered { .. } => "agent_registered",
            Self::AgentActivityChanged { .. } => "agent_activity_changed",
            Self::ResidentWorkStarted { .. } => "resident_work_started",
            Self::ResidentWorkFinished { .. } => "resident_work_finished",
            Self::AgentHeartbeat { .. } => "agent_heartbeat",
            Self::SubagentReported { .. } => "subagent_reported",
            Self::HandoffCommitted { .. } => "handoff_committed",
            Self::AgentArchived { .. } => "agent_archived",
            Self::AgentRestarted { .. } => "agent_restarted",
        }
    }

    /// Owning session id.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::ContextEvicted { session, .. }
            | Self::ContextStatus { session, .. }
            | Self::WorkflowSelected { session, .. }
            | Self::WorkflowRunStarted { session, .. }
            | Self::WorkflowStageStarted { session, .. }
            | Self::WorkflowStageMemberLinked { session, .. }
            | Self::WorkflowStageRouteOutcome { session, .. }
            | Self::WorkflowStageFinished { session, .. }
            | Self::WorkflowRunFinished { session, .. }
            | Self::MailSent { session, .. }
            | Self::ChannelJoined { session, .. }
            | Self::ChannelCreated { session, .. }
            | Self::MailConsumed { session, .. }
            | Self::SessionStatus { session, .. }
            | Self::MemberSpawned { session, .. }
            | Self::MemberStatusChanged { session, .. }
            | Self::MemberFinished { session, .. }
            | Self::AgentRegistered { session, .. }
            | Self::AgentActivityChanged { session, .. }
            | Self::ResidentWorkStarted { session, .. }
            | Self::ResidentWorkFinished { session, .. }
            | Self::AgentHeartbeat { session, .. }
            | Self::SubagentReported { session, .. }
            | Self::HandoffCommitted { session, .. }
            | Self::AgentArchived { session, .. }
            | Self::AgentRestarted { session, .. } => *session,
        }
    }

    /// Encode this typed extension event into [`Event::Extension`].
    #[must_use]
    pub fn into_event(self) -> Event {
        let session = self.session();
        let bundle = self.bundle_id().to_owned();
        let kind = self.kind().to_owned();
        let payload = match self {
            Self::ContextEvicted {
                evicted_parts,
                parts,
                tokens_before,
                tokens_after,
                threshold,
                ..
            } => to_value(ContextEvictedWire {
                evicted_parts,
                parts,
                tokens_before,
                tokens_after,
                threshold,
            }),
            Self::ContextStatus {
                tokens,
                source,
                mode,
                threshold,
                ..
            } => to_value(ContextStatusProjection {
                tokens,
                source,
                mode,
                threshold,
            }),
            Self::WorkflowSelected { workflow, .. } => to_value(WorkflowSelectedWire { workflow }),
            Self::WorkflowRunStarted {
                run,
                workflow,
                request_hash,
                owner,
                stages,
                ..
            } => to_value(WorkflowRunStartedWire {
                run,
                workflow,
                request_hash,
                owner,
                stages,
            }),
            Self::WorkflowStageStarted { run, stage, .. } => {
                to_value(WorkflowStageStartedWire { run, stage })
            }
            Self::WorkflowStageMemberLinked {
                run,
                stage,
                member,
                role,
                iteration,
                ..
            } => to_value(WorkflowStageMemberLinkedWire {
                run,
                stage,
                member,
                role,
                iteration,
            }),
            Self::WorkflowStageRouteOutcome {
                run,
                stage,
                member,
                role,
                iteration,
                step,
                candidate_index,
                model,
                reasoning,
                failure_class,
                ..
            } => to_value(WorkflowStageRouteOutcomeWire {
                run,
                stage,
                member,
                role,
                iteration,
                step,
                candidate_index,
                model,
                reasoning,
                failure_class,
            }),
            Self::WorkflowStageFinished {
                run, stage, status, ..
            } => to_value(WorkflowStageFinishedWire { run, stage, status }),
            Self::WorkflowRunFinished {
                run, status, error, ..
            } => to_value(WorkflowRunFinishedWire { run, status, error }),
            Self::MailSent {
                from,
                to,
                kind,
                body,
                ..
            } => to_value(MailSentWire {
                from,
                to,
                kind,
                body,
            }),
            Self::ChannelJoined {
                channel, member, ..
            } => to_value(ChannelJoinedWire { channel, member }),
            Self::ChannelCreated {
                channel,
                kind,
                unit,
                members,
                ..
            } => to_value(ChannelCreatedWire {
                channel,
                kind,
                unit,
                members,
            }),
            Self::MailConsumed {
                handle, through, ..
            } => to_value(MailConsumedWire { handle, through }),
            Self::SessionStatus { status, .. } => to_value(SessionStatusWire { status }),
            Self::MemberSpawned {
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                tool_call,
                ..
            } => to_value(MemberSpawnedWire {
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                tool_call,
            }),
            Self::MemberStatusChanged { member, status, .. } => {
                to_value(MemberStatusChangedWire { member, status })
            }
            Self::MemberFinished {
                member,
                status,
                summary,
                child,
                ..
            } => to_value(MemberFinishedWire {
                member,
                status,
                summary,
                child,
            }),
            Self::AgentRegistered {
                agent_session,
                handle,
                parent,
                agent_type,
                mode,
                ..
            } => to_value(AgentRegisteredWire {
                agent_session,
                handle,
                parent,
                agent_type,
                mode,
            }),
            Self::AgentActivityChanged {
                handle,
                status,
                current_task,
                ..
            } => to_value(AgentActivityChangedWire {
                handle,
                status,
                current_task,
            }),
            Self::ResidentWorkStarted {
                actor_session,
                handle,
                epoch,
                inbox_through,
                ..
            } => to_value(ResidentWorkStartedWire {
                actor_session,
                handle,
                epoch,
                inbox_through,
            }),
            Self::ResidentWorkFinished {
                handle,
                epoch,
                inbox_through,
                ..
            } => to_value(ResidentWorkFinishedWire {
                handle,
                epoch,
                inbox_through,
            }),
            Self::AgentHeartbeat {
                handle,
                heartbeat_ms,
                ..
            } => to_value(AgentHeartbeatWire {
                handle,
                heartbeat_ms,
            }),
            Self::SubagentReported {
                member,
                child,
                handle,
                outcome,
                report,
                ..
            } => to_value(SubagentReportedWire {
                member,
                child,
                handle,
                outcome,
                report,
            }),
            Self::HandoffCommitted {
                handle,
                generation,
                doc,
                degraded,
                ..
            } => to_value(HandoffCommittedWire {
                handle,
                generation,
                doc,
                degraded,
            }),
            Self::AgentArchived {
                handle,
                child,
                reason,
                ..
            } => to_value(AgentArchivedWire {
                handle,
                child,
                reason,
            }),
            Self::AgentRestarted {
                handle,
                child,
                epoch,
                ..
            } => to_value(AgentRestartedWire {
                handle,
                child,
                epoch,
            }),
        };
        Event::Extension {
            session,
            bundle,
            kind,
            payload,
        }
    }

    /// Decode an [`Event`] if it is a known domain extension event.
    #[must_use]
    pub fn from_event(event: &Event) -> Option<Self> {
        let Event::Extension {
            session,
            bundle,
            kind,
            payload,
        } = event
        else {
            return None;
        };
        Self::from_parts(*session, bundle, kind, payload)
    }

    /// Decode `(session, bundle, kind, payload)` into a typed [`ExtensionEvent`].
    #[must_use]
    pub fn from_parts(session: SessionId, bundle: &str, kind: &str, payload: &Value) -> Option<Self> {
        match (bundle, kind) {
            (BUNDLE_COMPACTION, "context_evicted") => {
                let wire: ContextEvictedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::ContextEvicted {
                    session,
                    evicted_parts: wire.evicted_parts,
                    parts: wire.parts,
                    tokens_before: wire.tokens_before,
                    tokens_after: wire.tokens_after,
                    threshold: wire.threshold,
                })
            }
            (BUNDLE_COMPACTION, "context_status") => {
                let wire: ContextStatusProjection = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::ContextStatus {
                    session,
                    tokens: wire.tokens,
                    source: wire.source,
                    mode: wire.mode,
                    threshold: wire.threshold,
                })
            }
            (BUNDLE_WORKFLOW, "workflow_selected") => {
                let wire: WorkflowSelectedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::WorkflowSelected {
                    session,
                    workflow: wire.workflow,
                })
            }
            (BUNDLE_WORKFLOW, "workflow_run_started") => {
                let wire: WorkflowRunStartedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::WorkflowRunStarted {
                    session,
                    run: wire.run,
                    workflow: wire.workflow,
                    request_hash: wire.request_hash,
                    owner: wire.owner,
                    stages: wire.stages,
                })
            }
            (BUNDLE_WORKFLOW, "workflow_stage_started") => {
                let wire: WorkflowStageStartedWire =
                    serde_json::from_value(payload.clone()).ok()?;
                Some(Self::WorkflowStageStarted {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                })
            }
            (BUNDLE_WORKFLOW, "workflow_stage_member_linked") => {
                let wire: WorkflowStageMemberLinkedWire =
                    serde_json::from_value(payload.clone()).ok()?;
                Some(Self::WorkflowStageMemberLinked {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                    member: wire.member,
                    role: wire.role,
                    iteration: wire.iteration,
                })
            }
            (BUNDLE_WORKFLOW, "workflow_stage_route_outcome") => {
                let wire: WorkflowStageRouteOutcomeWire =
                    serde_json::from_value(payload.clone()).ok()?;
                Some(Self::WorkflowStageRouteOutcome {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                    member: wire.member,
                    role: wire.role,
                    iteration: wire.iteration,
                    step: wire.step,
                    candidate_index: wire.candidate_index,
                    model: wire.model,
                    reasoning: wire.reasoning,
                    failure_class: wire.failure_class,
                })
            }
            (BUNDLE_WORKFLOW, "workflow_stage_finished") => {
                let wire: WorkflowStageFinishedWire =
                    serde_json::from_value(payload.clone()).ok()?;
                Some(Self::WorkflowStageFinished {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                    status: wire.status,
                })
            }
            (BUNDLE_WORKFLOW, "workflow_run_finished") => {
                let wire: WorkflowRunFinishedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::WorkflowRunFinished {
                    session,
                    run: wire.run,
                    status: wire.status,
                    error: wire.error,
                })
            }
            (BUNDLE_CHANNELS, "mail_sent") => {
                let wire: MailSentWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::MailSent {
                    session,
                    from: wire.from,
                    to: wire.to,
                    kind: wire.kind,
                    body: wire.body,
                })
            }
            (BUNDLE_CHANNELS, "channel_joined") => {
                let wire: ChannelJoinedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::ChannelJoined {
                    session,
                    channel: wire.channel,
                    member: wire.member,
                })
            }
            (BUNDLE_CHANNELS, "channel_created") => {
                let wire: ChannelCreatedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::ChannelCreated {
                    session,
                    channel: wire.channel,
                    kind: wire.kind,
                    unit: wire.unit,
                    members: wire.members,
                })
            }
            (BUNDLE_CHANNELS, "mail_consumed") => {
                let wire: MailConsumedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::MailConsumed {
                    session,
                    handle: wire.handle,
                    through: wire.through,
                })
            }
            (BUNDLE_MULTI_AGENT, "session_status") => {
                let wire: SessionStatusWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::SessionStatus {
                    session,
                    status: wire.status,
                })
            }
            (BUNDLE_MULTI_AGENT, "member_spawned") => {
                let wire: MemberSpawnedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::MemberSpawned {
                    session,
                    member: wire.member,
                    child: wire.child,
                    subagent_type: wire.subagent_type,
                    description: wire.description,
                    depth: wire.depth,
                    directive: wire.directive,
                    tool_call: wire.tool_call,
                })
            }
            (BUNDLE_MULTI_AGENT, "member_status_changed") => {
                let wire: MemberStatusChangedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::MemberStatusChanged {
                    session,
                    member: wire.member,
                    status: wire.status,
                })
            }
            (BUNDLE_MULTI_AGENT, "member_finished") => {
                let wire: MemberFinishedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::MemberFinished {
                    session,
                    member: wire.member,
                    status: wire.status,
                    summary: wire.summary,
                    child: wire.child,
                })
            }
            (BUNDLE_MULTI_AGENT, "agent_registered") => {
                let wire: AgentRegisteredWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::AgentRegistered {
                    session,
                    agent_session: wire.agent_session,
                    handle: wire.handle,
                    parent: wire.parent,
                    agent_type: wire.agent_type,
                    mode: wire.mode,
                })
            }
            (BUNDLE_MULTI_AGENT, "agent_activity_changed") => {
                let wire: AgentActivityChangedWire =
                    serde_json::from_value(payload.clone()).ok()?;
                Some(Self::AgentActivityChanged {
                    session,
                    handle: wire.handle,
                    status: wire.status,
                    current_task: wire.current_task,
                })
            }
            (BUNDLE_MULTI_AGENT, "resident_work_started") => {
                let wire: ResidentWorkStartedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::ResidentWorkStarted {
                    session,
                    actor_session: wire.actor_session,
                    handle: wire.handle,
                    epoch: wire.epoch,
                    inbox_through: wire.inbox_through,
                })
            }
            (BUNDLE_MULTI_AGENT, "resident_work_finished") => {
                let wire: ResidentWorkFinishedWire =
                    serde_json::from_value(payload.clone()).ok()?;
                Some(Self::ResidentWorkFinished {
                    session,
                    handle: wire.handle,
                    epoch: wire.epoch,
                    inbox_through: wire.inbox_through,
                })
            }
            (BUNDLE_MULTI_AGENT, "agent_heartbeat") => {
                let wire: AgentHeartbeatWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::AgentHeartbeat {
                    session,
                    handle: wire.handle,
                    heartbeat_ms: wire.heartbeat_ms,
                })
            }
            (BUNDLE_MULTI_AGENT, "subagent_reported") => {
                let wire: SubagentReportedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::SubagentReported {
                    session,
                    member: wire.member,
                    child: wire.child,
                    handle: wire.handle,
                    outcome: wire.outcome,
                    report: wire.report,
                })
            }
            (BUNDLE_MULTI_AGENT, "handoff_committed") => {
                let wire: HandoffCommittedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::HandoffCommitted {
                    session,
                    handle: wire.handle,
                    generation: wire.generation,
                    doc: wire.doc,
                    degraded: wire.degraded,
                })
            }
            (BUNDLE_MULTI_AGENT, "agent_archived") => {
                let wire: AgentArchivedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::AgentArchived {
                    session,
                    handle: wire.handle,
                    child: wire.child,
                    reason: wire.reason,
                })
            }
            (BUNDLE_MULTI_AGENT, "agent_restarted") => {
                let wire: AgentRestartedWire = serde_json::from_value(payload.clone()).ok()?;
                Some(Self::AgentRestarted {
                    session,
                    handle: wire.handle,
                    child: wire.child,
                    epoch: wire.epoch,
                })
            }
            _ => None,
        }
    }
}

fn read_extension<T: for<'de> Deserialize<'de> + Default>(
    projection: &Projection,
    bundle: &str,
) -> T {
    projection
        .extensions
        .get(bundle)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

fn write_extension<T: Serialize>(projection: &mut Projection, bundle: &str, value: &T) {
    if let Ok(serialized) = serde_json::to_value(value) {
        projection.extensions.insert(bundle.to_owned(), serialized);
    }
}

/// Extract folded team projection (`hya/agent-channels` + `hya/multi-agent`).
#[must_use]
pub fn team_projection(projection: &Projection) -> TeamProjection {
    let channels_state: ChannelsExtensionState = read_extension(projection, BUNDLE_CHANNELS);
    let multi_state: MultiAgentExtensionState = read_extension(projection, BUNDLE_MULTI_AGENT);
    TeamProjection {
        mail: channels_state.mail,
        inboxes: channels_state.inboxes,
        channels: channels_state.channels,
        roster: multi_state.roster,
        archived: multi_state.archived,
    }
}

/// Extract folded spawned members (`hya/multi-agent`).
#[must_use]
pub fn members_projection(projection: &Projection) -> Vec<MemberProjection> {
    read_extension::<MultiAgentExtensionState>(projection, BUNDLE_MULTI_AGENT).members
}

/// Extract latest folded terminal handoff (`hya/multi-agent`).
#[must_use]
pub fn handoff_projection(projection: &Projection) -> Option<HandoffProjection> {
    read_extension::<MultiAgentExtensionState>(projection, BUNDLE_MULTI_AGENT).handoff
}

/// Extract folded workflow projection (`hya/workflow`).
#[must_use]
pub fn workflow_projection(projection: &Projection) -> Option<WorkflowProjection> {
    projection
        .extensions
        .get(BUNDLE_WORKFLOW)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// Extract latest folded context status (`hya/compaction`).
#[must_use]
pub fn context_status_projection(projection: &Projection) -> Option<ContextStatusProjection> {
    read_extension::<CompactionExtensionState>(projection, BUNDLE_COMPACTION).context_status
}

/// Projection reducer for `hya/compaction`.
#[derive(Debug, Default)]
pub struct CompactionProjectionReducer;

impl ProjectionReducer for CompactionProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_COMPACTION
    }

    fn fold_extension(
        &self,
        projection: &mut Projection,
        session: SessionId,
        kind: &str,
        payload: &Value,
    ) {
        let Some(ext) = ExtensionEvent::from_parts(session, BUNDLE_COMPACTION, kind, payload)
        else {
            return;
        };
        match ext {
            ExtensionEvent::ContextEvicted { parts, .. } => {
                for evicted in parts {
                    if let Some(PartProjection::Tool {
                        eviction_notice, ..
                    }) = projection
                        .session
                        .messages
                        .iter_mut()
                        .rev()
                        .flat_map(|row| row.parts.iter_mut())
                        .find(|part| part.id() == evicted.part)
                    {
                        *eviction_notice = Some(evicted.notice);
                    }
                }
            }
            ExtensionEvent::ContextStatus {
                tokens,
                source,
                mode,
                threshold,
                ..
            } => {
                let state = CompactionExtensionState {
                    context_status: Some(ContextStatusProjection {
                        tokens,
                        source,
                        mode,
                        threshold,
                    }),
                };
                write_extension(projection, BUNDLE_COMPACTION, &state);
            }
            _ => {}
        }
    }
}

/// Projection reducer for `hya/workflow`.
#[derive(Debug, Default)]
pub struct WorkflowProjectionReducer;

impl ProjectionReducer for WorkflowProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_WORKFLOW
    }

    fn fold_extension(
        &self,
        projection: &mut Projection,
        session: SessionId,
        kind: &str,
        payload: &Value,
    ) {
        let Some(ext) = ExtensionEvent::from_parts(session, BUNDLE_WORKFLOW, kind, payload) else {
            return;
        };
        let mut wf: WorkflowProjection = workflow_projection(projection).unwrap_or_default();
        match ext {
            ExtensionEvent::WorkflowSelected { workflow, .. } => {
                wf.selection = Some(workflow);
            }
            ExtensionEvent::WorkflowRunStarted {
                run,
                workflow,
                request_hash,
                owner,
                stages,
                ..
            } => {
                if wf.seen_runs.insert(run) {
                    wf.selection = Some(workflow.clone());
                    wf.run = Some(WorkflowRunProjection {
                        id: run,
                        workflow,
                        request_hash,
                        owner,
                        status: WorkflowRunStatus::Running,
                        stages: stages
                            .into_iter()
                            .map(|plan| WorkflowStageProjection {
                                plan,
                                status: WorkflowStageStatus::Pending,
                                members: Vec::new(),
                                route_outcomes: Vec::new(),
                            })
                            .collect(),
                        error: None,
                    });
                }
            }
            ExtensionEvent::WorkflowStageStarted { run, stage, .. } => {
                if let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(existing) = current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && !existing.status.is_terminal()
                {
                    existing.status = WorkflowStageStatus::Running;
                }
            }
            ExtensionEvent::WorkflowStageMemberLinked {
                run,
                stage,
                member,
                role,
                iteration,
                ..
            } => {
                if let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(stage_row) = current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && !stage_row
                        .members
                        .iter()
                        .any(|row| row.member == member && row.role == role && row.iteration == iteration)
                {
                    stage_row.members.push(WorkflowMemberProjection {
                        member,
                        role,
                        iteration,
                    });
                }
            }
            ExtensionEvent::WorkflowStageRouteOutcome {
                session,
                run,
                stage,
                member,
                role,
                iteration,
                step,
                candidate_index,
                model,
                reasoning,
                failure_class,
            } => {
                if let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(stage_row) =
                        current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && stage_row.status == WorkflowStageStatus::Running
                    && !stage_row.route_outcomes.iter().any(|outcome| {
                        outcome.stage == stage
                            && outcome.member == member
                            && outcome.role == role
                            && outcome.iteration == iteration
                            && outcome.step == step
                    })
                {
                    stage_row.route_outcomes.push(WorkflowStageRouteOutcome {
                        session,
                        run,
                        stage,
                        member,
                        role,
                        iteration,
                        step,
                        candidate_index,
                        model,
                        reasoning,
                        failure_class,
                    });
                }
            }
            ExtensionEvent::WorkflowStageFinished {
                run, stage, status, ..
            } => {
                if status.is_terminal()
                    && let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(stage_row) =
                        current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && !stage_row.status.is_terminal()
                {
                    stage_row.status = status;
                }
            }
            ExtensionEvent::WorkflowRunFinished {
                run, status, error, ..
            } => {
                if status.is_terminal()
                    && let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && !current.status.is_terminal()
                {
                    current.status = status;
                    current.error = error;
                    if status != WorkflowRunStatus::Completed {
                        for stage in &mut current.stages {
                            if stage.status == WorkflowStageStatus::Pending {
                                stage.status = WorkflowStageStatus::Skipped;
                            } else if stage.status == WorkflowStageStatus::Running {
                                stage.status = if status == WorkflowRunStatus::Cancelled {
                                    WorkflowStageStatus::Cancelled
                                } else {
                                    WorkflowStageStatus::Failed
                                };
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        write_extension(projection, BUNDLE_WORKFLOW, &wf);
    }
}

/// Projection reducer for `hya/agent-channels`.
#[derive(Debug, Default)]
pub struct AgentChannelsProjectionReducer;

impl ProjectionReducer for AgentChannelsProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_CHANNELS
    }

    fn fold_extension(
        &self,
        projection: &mut Projection,
        session: SessionId,
        kind: &str,
        payload: &Value,
    ) {
        let Some(ext) = ExtensionEvent::from_parts(session, BUNDLE_CHANNELS, kind, payload) else {
            return;
        };
        let mut state: ChannelsExtensionState = read_extension(projection, BUNDLE_CHANNELS);
        let mut multi: MultiAgentExtensionState = read_extension(projection, BUNDLE_MULTI_AGENT);
        match ext {
            ExtensionEvent::ChannelJoined { .. } => {}
            ExtensionEvent::ChannelCreated {
                channel,
                kind,
                unit,
                members,
                ..
            } => {
                let channel_state = state.channels.entry(channel).or_default();
                channel_state.kind = kind;
                if unit.is_some() {
                    channel_state.unit = unit;
                }
                channel_state.members.extend(members);
                write_extension(projection, BUNDLE_CHANNELS, &state);
            }
            ExtensionEvent::MailSent {
                from,
                to,
                kind,
                body,
                ..
            } => {
                let message = MailMessage {
                    from,
                    to: to.clone(),
                    kind,
                    body,
                };
                let index = state.mail.len();
                state.mail.push(message);
                match to {
                    MailEndpoint::Handle(handle) => {
                        state.inboxes.entry(handle).or_default().push(index);
                    }
                    MailEndpoint::Channel(channel) => {
                        let channel_state = state.channels.entry(channel).or_default();
                        channel_state.log.push(index);
                        let members: Vec<String> = channel_state.members.iter().cloned().collect();
                        for member in members {
                            let is_live = multi
                                .roster
                                .get(&member)
                                .is_none_or(RosterEntry::accepts_mail);
                            if !is_live {
                                continue;
                            }
                            state.inboxes.entry(member).or_default().push(index);
                        }
                    }
                }
                write_extension(projection, BUNDLE_CHANNELS, &state);
            }
            ExtensionEvent::MailConsumed {
                handle, through, ..
            } => {
                if let Some(entry) = multi.roster.get_mut(&handle) {
                    entry.resident_cursor = entry.resident_cursor.max(through);
                    write_extension(projection, BUNDLE_MULTI_AGENT, &multi);
                }
            }
            _ => {}
        }
    }
}

/// Projection reducer for `hya/multi-agent`.
#[derive(Debug, Default)]
pub struct MultiAgentProjectionReducer;

fn member_mut(members: &mut Vec<MemberProjection>, member: MemberId) -> &mut MemberProjection {
    if let Some(idx) = members.iter().position(|m| m.member == member) {
        return &mut members[idx];
    }
    members.push(MemberProjection {
        member,
        child: None,
        subagent_type: AgentName::new(""),
        description: String::new(),
        depth: 0,
        status: MemberRunStatus::Spawning,
        summary: String::new(),
        directive: String::new(),
        tool_call: None,
    });
    let last = members.len() - 1;
    &mut members[last]
}

impl ProjectionReducer for MultiAgentProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_MULTI_AGENT
    }

    fn fold_extension(
        &self,
        projection: &mut Projection,
        session: SessionId,
        kind: &str,
        payload: &Value,
    ) {
        let Some(ext) = ExtensionEvent::from_parts(session, BUNDLE_MULTI_AGENT, kind, payload)
        else {
            return;
        };
        let mut state: MultiAgentExtensionState = read_extension(projection, BUNDLE_MULTI_AGENT);
        let mut channels: ChannelsExtensionState = read_extension(projection, BUNDLE_CHANNELS);
        let mut channels_dirty = false;
        let mut multi_dirty = true;

        match ext {
            ExtensionEvent::SessionStatus { .. } => {
                multi_dirty = false;
            }
            ExtensionEvent::MemberSpawned {
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                tool_call,
                ..
            } => {
                let entry = member_mut(&mut state.members, member);
                entry.child = child;
                entry.subagent_type = subagent_type;
                entry.description = description;
                entry.depth = depth;
                entry.status = MemberRunStatus::Spawning;
                if !directive.is_empty() {
                    entry.directive = directive;
                }
                if tool_call.is_some() {
                    entry.tool_call = tool_call;
                }
            }
            ExtensionEvent::MemberStatusChanged { member, status, .. } => {
                member_mut(&mut state.members, member).status = status;
            }
            ExtensionEvent::MemberFinished {
                member,
                status,
                summary,
                child,
                ..
            } => {
                let entry = member_mut(&mut state.members, member);
                entry.status = status;
                entry.summary = summary;
                if child.is_some() {
                    entry.child = child;
                }
            }
            ExtensionEvent::AgentRegistered {
                agent_session,
                handle,
                parent,
                agent_type,
                mode,
                ..
            } => {
                let path = match &parent {
                    Some(parent) => scope::join_path(parent, &handle),
                    None => handle,
                };
                if state.archived.contains_key(&path)
                    && !state.roster.contains_key(&path)
                    && let Some(parent) = parent
                    && let Some(channel) = channels.channels.values_mut().find(|channel| {
                        channel.kind == ChannelKind::Group && channel.unit.as_ref() == Some(&parent)
                    })
                {
                    channel.members.insert(path.clone());
                    channels_dirty = true;
                }
                let cursor = channels
                    .inboxes
                    .get(&path)
                    .map_or(0, |inbox| inbox.len() as u64);
                let entry = state.roster.entry(path.clone()).or_insert_with(|| RosterEntry {
                    handle: path,
                    session: agent_session,
                    agent_type: agent_type.clone(),
                    mode,
                    status: RosterStatus::default(),
                    current_task: None,
                    resident_cursor: cursor,
                    heartbeat_ms: 0,
                    resident_work: None,
                });
                entry.session = agent_session;
                entry.agent_type = agent_type;
                entry.mode = mode;
            }
            ExtensionEvent::AgentActivityChanged {
                handle,
                status,
                current_task,
                ..
            } => {
                if let Some(entry) = state.roster.get_mut(&handle) {
                    entry.status = status;
                    if current_task.is_some() {
                        entry.current_task = current_task;
                    }
                }
            }
            ExtensionEvent::ResidentWorkStarted {
                actor_session,
                handle,
                epoch,
                inbox_through,
                ..
            } => {
                if let Some(entry) = state.roster.get_mut(&handle)
                    && entry.session == actor_session
                    && entry.mode.is_resident()
                {
                    entry.status = RosterStatus::Busy;
                    entry.resident_work = Some(ResidentWorkProjection {
                        epoch,
                        inbox_through,
                    });
                }
            }
            ExtensionEvent::ResidentWorkFinished {
                handle,
                epoch,
                inbox_through,
                ..
            } => {
                if let Some(entry) = state.roster.get_mut(&handle)
                    && entry.resident_work.is_some_and(|work| work.epoch == epoch)
                {
                    entry.resident_work = None;
                    entry.resident_cursor = entry.resident_cursor.max(inbox_through);
                }
            }
            ExtensionEvent::AgentHeartbeat {
                handle,
                heartbeat_ms,
                ..
            } => {
                if let Some(entry) = state.roster.get_mut(&handle) {
                    entry.heartbeat_ms = entry.heartbeat_ms.max(heartbeat_ms);
                }
            }
            ExtensionEvent::SubagentReported {
                member,
                outcome,
                report,
                ..
            } => {
                let entry = member_mut(&mut state.members, member);
                entry.status = match outcome {
                    ReportOutcome::Done => MemberRunStatus::Done,
                    ReportOutcome::Failed => MemberRunStatus::Failed,
                };
                entry.summary = report;
            }
            ExtensionEvent::HandoffCommitted {
                generation,
                doc,
                degraded,
                ..
            } => {
                state.handoff = Some(HandoffProjection {
                    generation,
                    doc,
                    degraded,
                });
            }
            ExtensionEvent::AgentArchived {
                handle,
                child,
                reason,
                ..
            } => {
                if let Some(entry) = state.roster.remove(&handle) {
                    state.archived.insert(
                        handle.clone(),
                        ArchivedEntry {
                            handle: handle.clone(),
                            session: child,
                            agent_type: entry.agent_type,
                            reason,
                        },
                    );
                }
                for channel in channels.channels.values_mut() {
                    if channel.kind == ChannelKind::Group {
                        channel.members.remove(&handle);
                        channels_dirty = true;
                    }
                }
            }
            ExtensionEvent::AgentRestarted { handle, .. } => {
                state.archived.remove(&handle);
            }
            _ => {
                multi_dirty = false;
            }
        }

        if multi_dirty {
            write_extension(projection, BUNDLE_MULTI_AGENT, &state);
        }
        if channels_dirty {
            write_extension(projection, BUNDLE_CHANNELS, &channels);
        }
    }
}

/// Build the full Phase 0 [`ProjectionReducerRegistry`] for compaction, workflow,
/// agent-channels, and multi-agent extension events.
#[must_use]
pub fn projection_reducers() -> ProjectionReducerRegistry {
    ProjectionReducerRegistry::new()
        .with_reducer(Arc::new(CompactionProjectionReducer))
        .with_reducer(Arc::new(WorkflowProjectionReducer))
        .with_reducer(Arc::new(AgentChannelsProjectionReducer))
        .with_reducer(Arc::new(MultiAgentProjectionReducer))
}
