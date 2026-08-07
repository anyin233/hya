//! Shared idempotent reducer: fold an event log into a session view. Used by the
//! store (read path) and the client (SSE reconnect); idempotent by `EventSeq` so
//! re-delivered events are no-ops.

mod helpers;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use self::helpers::{find_part, push_part, tool_input, upsert_tool};
use crate::event::{Envelope, Event};
use crate::ids::{
    ActorEpoch, ConfigGeneration, MemberId, MessageId, PartId, SessionId, ToolCallId,
};
use crate::mail::{MailEndpoint, MailKind};
use crate::message::{
    FinishReason, MemberRunStatus, Role, RosterStatus, SubagentMode, TokenUsage, ToolPartState,
};
use crate::model::{AgentName, ModelRef, ToolName};

/// Folded view of one session's transcript and metadata (not the team mailbox).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionProjection {
    /// Session id once `SessionCreated` has been applied.
    pub id: Option<SessionId>,
    /// Parent session for subagent lineage (toward team root).
    pub parent: Option<SessionId>,
    /// Current agent binding.
    pub agent: Option<AgentName>,
    /// Current model binding.
    pub model: Option<ModelRef>,
    /// Absolute workdir for tools.
    pub workdir: Option<String>,
    /// Display title when set.
    pub title: Option<String>,
    /// Opaque session metadata object.
    pub metadata: Option<serde_json::Value>,
    /// Full permission rule list (last `SessionPermissionSet` wins).
    pub permission: Option<Vec<serde_json::Value>>,
    /// Archive stamp when archived.
    pub archived: Option<serde_json::Number>,
    /// Share URL when shared; `None` after clear.
    pub share: Option<String>,
    /// Ordered transcript messages.
    pub messages: Vec<MessageProjection>,
    /// Subagents spawned by this session, folded from member lifecycle events.
    /// Empty for sessions that never spawned subagents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberProjection>,
}

/// A single spawned subagent as seen from its parent session. Carries only bounded
/// metadata + a short summary (never the child transcript), so a recursive run tree
/// can be assembled cheaply by joining `child` links across sessions.
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
    /// Bounded finish summary (never the full child transcript).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Verbatim parent directive that defines this member's purpose.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub directive: String,
    /// Tool call that caused this spawn, when it came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCallId>,
}

/// One message row in a folded session transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageProjection {
    /// Message id.
    pub id: MessageId,
    /// Speaker role.
    pub role: Role,
    /// Runtime snapshot generation from `TurnBindingRecorded` (assistant turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_generation: Option<ConfigGeneration>,
    /// Finish reason when the message is closed.
    pub finish: Option<FinishReason>,
    /// Usage recorded on `MessageFinished`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    /// Prompt file attachments from `UserPromptContextRecorded`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<serde_json::Value>,
    /// Prompt agent mentions from `UserPromptContextRecorded`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<serde_json::Value>,
    /// Ordered content parts (text / reasoning / tool only — no media).
    pub parts: Vec<PartProjection>,
}

/// Projected content part (wire tag `kind`). No media variant — media does not survive fold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PartProjection {
    /// Accumulated plain text.
    Text {
        /// Part id.
        id: PartId,
        /// Full text so far.
        text: String,
    },
    /// Accumulated reasoning plus optional provider blob.
    Reasoning {
        /// Part id.
        id: PartId,
        /// Full reasoning text so far.
        text: String,
        /// Opaque provider state to round-trip on the next request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    /// Tool call with streaming state.
    Tool {
        /// Part id.
        id: PartId,
        /// Tool call id.
        call: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
        /// Current tool phase and payloads.
        state: ToolPartState,
    },
}

impl PartProjection {
    /// Stable part id shared by all variants.
    #[must_use]
    pub fn id(&self) -> PartId {
        match self {
            PartProjection::Text { id, .. }
            | PartProjection::Reasoning { id, .. }
            | PartProjection::Tool { id, .. } => *id,
        }
    }
}

/// Team-scoped mailbox/channel state (ADR-0001), folded from the team-root log.
///
/// A replay of the root session's event log reconstructs this exactly: handles
/// are baked into `AgentRegistered`, channel membership into `ChannelJoined`/
/// `ChannelLeft`, and every `MailSent` is appended (in seq order) to the
/// recipient inboxes / channel log the reducer resolves at fold time. Kept on the
/// top-level [`Projection`] rather than [`SessionProjection`] because it belongs
/// to the whole team tree, not one session's transcript.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamProjection {
    /// Per-agent inbox keyed by handle, in delivery (seq) order. A direct message
    /// lands in the recipient handle's inbox; a channel message lands in every
    /// current subscriber's inbox.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inboxes: BTreeMap<String, Vec<MailMessage>>,
    /// Channels keyed by name (no leading `#`): membership set + full message log.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, ChannelProjection>,
    /// Live team roster keyed by handle.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roster: BTreeMap<String, RosterEntry>,
}

/// One channel: its current subscribers plus the ordered log of everything posted
/// to it (independent of who was subscribed when).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelProjection {
    /// Current subscriber handles (no leading `#`).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub members: BTreeSet<String>,
    /// Full ordered log of posts to this channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log: Vec<MailMessage>,
}

/// A single delivered message as folded into an inbox / channel log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MailMessage {
    /// Sender handle.
    pub from: String,
    /// Original address (handle or channel).
    pub to: MailEndpoint,
    /// Chatter vs announcement.
    #[serde(default)]
    pub kind: MailKind,
    /// Message body.
    pub body: String,
}

/// A live team member: its handle, own session, and declared agent type, plus the
/// resident-lifecycle enrichment (ADR-0002): its scheduling mode, live activity,
/// and the short current-task label the TUI shows.
///
/// `mode`/`status`/`current_task` are `#[serde(default)]` so logs written before
/// Phase 4 replay as transient, idle, and task-less.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RosterEntry {
    /// Stable team-scoped handle (mail address and UI label).
    pub handle: String,
    /// Agent's own session id.
    pub session: SessionId,
    /// Declared agent type for display.
    #[serde(default)]
    pub agent_type: AgentName,
    /// Transient vs resident scheduling (from `AgentRegistered`).
    #[serde(default)]
    pub mode: SubagentMode,
    /// Live activity, updated by `AgentActivityChanged` as the resident
    /// supervisor drives the member.
    #[serde(default)]
    pub status: RosterStatus,
    /// A short human-facing description of what the member is currently doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    /// Number of inbox messages durably consumed by terminal resident turns.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resident_cursor: u64,
    /// Present only after work starts and before it reaches a terminal/idle state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_work: Option<ResidentWorkProjection>,
}

/// In-flight resident turn boundary folded from `ResidentWorkStarted`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidentWorkProjection {
    /// Actor epoch that started this work.
    pub epoch: ActorEpoch,
    /// Inbox length covered by the coalesced turn (cursor advances on terminal/idle).
    pub inbox_through: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Full folded view: one session transcript plus optional team mailbox/roster state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    /// Session transcript and metadata.
    pub session: SessionProjection,
    /// Team-scoped mailbox/channel/roster state. Empty on sessions that are not a
    /// team root / carry no mail events.
    #[serde(default, skip_serializing_if = "team_is_empty")]
    pub team: TeamProjection,
    /// Highest durable `EventSeq` applied; live `seq == 0` does not advance this.
    pub last_seq: u64,
}

fn team_is_empty(team: &TeamProjection) -> bool {
    team.inboxes.is_empty() && team.channels.is_empty() && team.roster.is_empty()
}

impl Projection {
    /// Fold a slice of envelopes in order into a fresh projection.
    #[must_use]
    pub fn from_events(envs: &[Envelope]) -> Self {
        let mut p = Self::default();
        for e in envs {
            p.apply(e);
        }
        p
    }

    /// Apply one envelope: `seq == 0` is live-only (no `last_seq` advance);
    /// `seq <= last_seq` is a durable no-op; otherwise fold and advance `last_seq`.
    pub fn apply(&mut self, env: &Envelope) {
        if env.seq.0 == 0 {
            self.apply_event(&env.event);
            return;
        }
        if env.seq.0 <= self.last_seq {
            return;
        }
        self.apply_event(&env.event);
        self.last_seq = env.seq.0;
    }

    fn message_mut(&mut self, id: MessageId) -> Option<&mut MessageProjection> {
        self.session.messages.iter_mut().find(|m| m.id == id)
    }

    fn apply_event(&mut self, e: &Event) {
        match e {
            Event::SessionCreated {
                session,
                parent,
                agent,
                model,
                workdir,
                ..
            } => {
                self.session.id = Some(*session);
                self.session.parent = *parent;
                self.session.agent = Some(agent.clone());
                self.session.model = Some(model.clone());
                self.session.workdir = Some(workdir.clone());
            }
            Event::SessionMoved { workdir, .. } => {
                self.session.workdir = Some(workdir.clone());
            }
            Event::SessionTitled { title, .. } => {
                self.session.title = Some(title.clone());
            }
            Event::SessionMetadataSet { metadata, .. } => {
                self.session.metadata = Some(metadata.clone());
            }
            Event::SessionPermissionSet { permission, .. } => {
                self.session.permission = Some(permission.clone());
            }
            Event::SessionArchived { archived, .. } => {
                self.session.archived = Some(archived.clone());
            }
            Event::SessionShareSet { url, .. } => {
                self.session.share = Some(url.clone());
            }
            Event::SessionShareCleared { .. } => {
                self.session.share = None;
            }
            Event::AgentSwitched { agent, .. } => {
                self.session.agent = Some(agent.clone());
            }
            Event::ModelSwitched { model, .. } => {
                self.session.model = Some(model.clone());
            }
            Event::MessageStarted { message, role, .. } => {
                if self.message_mut(*message).is_none() {
                    self.session.messages.push(MessageProjection {
                        id: *message,
                        role: *role,
                        config_generation: None,
                        finish: None,
                        tokens: None,
                        files: Vec::new(),
                        agents: Vec::new(),
                        parts: Vec::new(),
                    });
                }
            }
            Event::TurnBindingRecorded {
                message,
                generation,
                ..
            } => {
                if let Some(message) = self.message_mut(*message) {
                    message.config_generation = Some(*generation);
                }
            }
            Event::UserPromptContextRecorded {
                message,
                files,
                agents,
                ..
            } => {
                if let Some(message) = self.message_mut(*message) {
                    message.files = files.clone();
                    message.agents = agents.clone();
                }
            }
            Event::MessageFinished {
                message,
                finish,
                tokens,
                ..
            } => {
                if let Some(m) = self.message_mut(*message) {
                    m.finish = Some(*finish);
                    m.tokens = *tokens;
                }
            }
            Event::MessageDeleted { message, .. } => {
                self.session.messages.retain(|item| item.id != *message);
            }
            Event::PartDeleted { message, part, .. } => {
                if let Some(message) = self.message_mut(*message) {
                    message.parts.retain(|item| item.id() != *part);
                }
            }
            Event::TextStart { message, part, .. } => push_part(
                self,
                *message,
                PartProjection::Text {
                    id: *part,
                    text: String::new(),
                },
            ),
            Event::TextDelta {
                message,
                part,
                delta,
                ..
            } => {
                if let Some(PartProjection::Text { text, .. }) = find_part(self, *message, *part) {
                    text.push_str(delta);
                }
            }
            Event::TextReplace {
                message,
                part,
                text: replacement,
                ..
            } => {
                if let Some(PartProjection::Text { text, .. }) = find_part(self, *message, *part) {
                    *text = replacement.clone();
                }
            }
            Event::ReasoningStart { message, part, .. } => push_part(
                self,
                *message,
                PartProjection::Reasoning {
                    id: *part,
                    text: String::new(),
                    provider_data: None,
                },
            ),
            Event::ReasoningDelta {
                message,
                part,
                delta,
                ..
            } => {
                if let Some(PartProjection::Reasoning { text, .. }) =
                    find_part(self, *message, *part)
                {
                    text.push_str(delta);
                }
            }
            Event::ReasoningReplace {
                message,
                part,
                text: replacement,
                ..
            } => {
                if let Some(PartProjection::Reasoning { text, .. }) =
                    find_part(self, *message, *part)
                {
                    *text = replacement.clone();
                }
            }
            Event::ReasoningEnd {
                message,
                part,
                provider_data,
                ..
            } => {
                if let Some(PartProjection::Reasoning {
                    provider_data: stored,
                    ..
                }) = find_part(self, *message, *part)
                {
                    *stored = provider_data.clone();
                }
            }
            Event::ToolInputStart {
                message,
                part,
                call,
                name,
                ..
            } => push_part(
                self,
                *message,
                PartProjection::Tool {
                    id: *part,
                    call: *call,
                    name: name.clone(),
                    state: ToolPartState::Pending {
                        input: serde_json::Value::Null,
                    },
                },
            ),
            Event::ToolCallRequested {
                message,
                part,
                call,
                name,
                input,
                ..
            } => upsert_tool(
                self,
                *message,
                *part,
                *call,
                name.clone(),
                ToolPartState::Running {
                    input: input.clone(),
                },
            ),
            Event::ToolResult {
                message,
                part,
                output,
                time_ms,
                ..
            } => {
                if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
                    let input = tool_input(state);
                    *state = ToolPartState::Completed {
                        input,
                        output: output.clone(),
                        time_ms: *time_ms,
                    };
                }
            }
            Event::ToolError {
                message,
                part,
                message_text,
                value,
                ..
            } => {
                if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
                    let input = tool_input(state);
                    *state = ToolPartState::Error {
                        input,
                        message: message_text.clone(),
                        value: value.clone(),
                    };
                }
            }
            Event::ToolPartUpdated {
                message,
                part,
                state: next,
                ..
            } => {
                if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
                    *state = next.clone();
                }
            }
            Event::MemberSpawned {
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                tool_call,
                ..
            } => {
                let entry = self.member_mut(*member);
                entry.child = *child;
                entry.subagent_type = subagent_type.clone();
                entry.description = description.clone();
                entry.depth = *depth;
                entry.status = MemberRunStatus::Spawning;
                // A resume re-emits MemberSpawned for the same member; keep the
                // original purpose rather than blanking it from a thinner re-emit.
                if !directive.is_empty() {
                    entry.directive = directive.clone();
                }
                if tool_call.is_some() {
                    entry.tool_call = *tool_call;
                }
            }
            Event::MemberStatusChanged { member, status, .. } => {
                self.member_mut(*member).status = *status;
            }
            Event::MemberFinished {
                member,
                status,
                summary,
                child,
                ..
            } => {
                let entry = self.member_mut(*member);
                entry.status = *status;
                entry.summary = summary.clone();
                if child.is_some() {
                    entry.child = *child;
                }
            }
            Event::AgentRegistered {
                agent_session,
                handle,
                agent_type,
                mode,
                ..
            } => {
                // Re-registering an existing handle preserves its live status /
                // current_task (the reducer keys the roster by handle) while
                // refreshing the binding + mode.
                let entry = self
                    .team
                    .roster
                    .entry(handle.clone())
                    .or_insert_with(|| RosterEntry {
                        handle: handle.clone(),
                        session: *agent_session,
                        agent_type: agent_type.clone(),
                        mode: *mode,
                        status: RosterStatus::default(),
                        current_task: None,
                        resident_cursor: 0,
                        resident_work: None,
                    });
                entry.session = *agent_session;
                entry.agent_type = agent_type.clone();
                entry.mode = *mode;
            }
            Event::AgentActivityChanged {
                handle,
                status,
                current_task,
                ..
            } => {
                let terminal = matches!(*status, RosterStatus::Done | RosterStatus::Failed);
                let explicit_stop = *status == RosterStatus::Failed
                    && current_task.as_deref() == Some("resident stopped");
                let terminal_inbox_len = if explicit_stop {
                    self.team
                        .inboxes
                        .get(handle)
                        .map_or(0, |inbox| inbox.len() as u64)
                } else {
                    0
                };
                if let Some(entry) = self.team.roster.get_mut(handle) {
                    entry.status = *status;
                    if current_task.is_some() {
                        entry.current_task = current_task.clone();
                    }
                    if *status != RosterStatus::Busy
                        && let Some(work) = entry.resident_work.take()
                    {
                        entry.resident_cursor = entry.resident_cursor.max(work.inbox_through);
                    }
                    if terminal && explicit_stop && entry.mode.is_resident() {
                        entry.resident_cursor = entry.resident_cursor.max(terminal_inbox_len);
                    }
                }
            }
            Event::ResidentWorkStarted {
                actor_session,
                handle,
                epoch,
                inbox_through,
                ..
            } => {
                if let Some(entry) = self.team.roster.get_mut(handle)
                    && entry.session == *actor_session
                    && entry.mode.is_resident()
                {
                    entry.resident_work = Some(ResidentWorkProjection {
                        epoch: *epoch,
                        inbox_through: *inbox_through,
                    });
                }
            }
            Event::ChannelJoined {
                channel, member, ..
            } => {
                self.team
                    .channels
                    .entry(channel.clone())
                    .or_default()
                    .members
                    .insert(member.clone());
            }
            Event::ChannelLeft {
                channel, member, ..
            } => {
                if let Some(ch) = self.team.channels.get_mut(channel) {
                    ch.members.remove(member);
                }
            }
            Event::MailSent {
                from,
                to,
                kind,
                body,
                ..
            } => {
                let message = MailMessage {
                    from: from.clone(),
                    to: to.clone(),
                    kind: *kind,
                    body: body.clone(),
                };
                match to {
                    MailEndpoint::Handle(handle) => {
                        self.team
                            .inboxes
                            .entry(handle.clone())
                            .or_default()
                            .push(message);
                    }
                    MailEndpoint::Channel(channel) => {
                        let channel_state = self.team.channels.entry(channel.clone()).or_default();
                        channel_state.log.push(message.clone());
                        // Fan out to every CURRENT subscriber. Snapshot the member
                        // set first so the inbox borrow does not alias the channel.
                        let members: Vec<String> = channel_state.members.iter().cloned().collect();
                        for member in members {
                            if self.team.roster.get(&member).is_some_and(|entry| {
                                entry.mode.is_resident()
                                    && matches!(
                                        entry.status,
                                        RosterStatus::Done | RosterStatus::Failed
                                    )
                            }) {
                                continue;
                            }
                            self.team
                                .inboxes
                                .entry(member)
                                .or_default()
                                .push(message.clone());
                        }
                    }
                }
            }
            Event::TextEnd { .. }
            | Event::SessionStatus { .. }
            | Event::ToolInputDelta { .. }
            | Event::CommandExecuted { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::Error { .. }
            // Observability record, not a state transition: the folded messages
            // stay in the log and the marker System message carries the output.
            | Event::ContextCompacted { .. }
            | Event::SessionForked { .. }
            | Event::Unknown => {}
        }
    }

    /// Get or insert the member projection for `member`.
    fn member_mut(&mut self, member: MemberId) -> &mut MemberProjection {
        if let Some(idx) = self.session.members.iter().position(|m| m.member == member) {
            return &mut self.session.members[idx];
        }
        self.session.members.push(MemberProjection {
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
        let last = self.session.members.len() - 1;
        &mut self.session.members[last]
    }
}

#[cfg(test)]
mod member_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ids::EventSeq;

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    #[test]
    fn folds_member_lifecycle_into_projection() {
        let parent = SessionId::new();
        let child = SessionId::new();
        let member = MemberId::new();
        let mut p = Projection::default();
        p.apply(&env(
            1,
            Event::MemberSpawned {
                session: parent,
                member,
                child: Some(child),
                subagent_type: AgentName::new("explore"),
                description: "scan routing".to_string(),
                depth: 1,
                directive: "Scan the routing layer and report every handler".to_string(),
                tool_call: Some(ToolCallId::new()),
            },
        ));
        p.apply(&env(
            2,
            Event::MemberStatusChanged {
                session: parent,
                member,
                status: MemberRunStatus::Running,
            },
        ));
        assert_eq!(p.session.members.len(), 1);
        assert_eq!(p.session.members[0].status, MemberRunStatus::Running);
        assert_eq!(p.session.members[0].child, Some(child));
        assert_eq!(
            p.session.members[0].subagent_type,
            AgentName::new("explore")
        );
        // The spawn edge carries the member's purpose and its originating call.
        assert_eq!(
            p.session.members[0].directive,
            "Scan the routing layer and report every handler"
        );
        assert!(p.session.members[0].tool_call.is_some());

        p.apply(&env(
            3,
            Event::MemberFinished {
                session: parent,
                member,
                status: MemberRunStatus::Done,
                summary: "found it".to_string(),
                child: Some(child),
            },
        ));
        assert_eq!(p.session.members.len(), 1, "same member upserts, not dupes");
        assert_eq!(p.session.members[0].status, MemberRunStatus::Done);
        assert_eq!(p.session.members[0].summary, "found it");

        // Idempotent by seq: replaying an older seq is a no-op.
        p.apply(&env(
            2,
            Event::MemberStatusChanged {
                session: parent,
                member,
                status: MemberRunStatus::Running,
            },
        ));
        assert_eq!(p.session.members[0].status, MemberRunStatus::Done);
    }
}

#[cfg(test)]
mod team_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ids::EventSeq;

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    /// The full team event log for the invariant: two agents join `#build`, a
    /// third posts to it, plus one direct message. Reused by both the fold test
    /// and the replay test so they are provably reconstructing the SAME state.
    fn build_log(root: SessionId) -> Vec<Envelope> {
        let alice = SessionId::new();
        let bob = SessionId::new();
        vec![
            env(
                1,
                Event::AgentRegistered {
                    session: root,
                    agent_session: alice,
                    handle: "reviewer-1".to_string(),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                2,
                Event::AgentRegistered {
                    session: root,
                    agent_session: bob,
                    handle: "reviewer-2".to_string(),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                3,
                Event::ChannelJoined {
                    session: root,
                    channel: "build".to_string(),
                    member: "reviewer-1".to_string(),
                },
            ),
            env(
                4,
                Event::ChannelJoined {
                    session: root,
                    channel: "build".to_string(),
                    member: "reviewer-2".to_string(),
                },
            ),
            env(
                5,
                Event::MailSent {
                    session: root,
                    from: "main".to_string(),
                    to: MailEndpoint::Channel("build".to_string()),
                    kind: MailKind::Announcement,
                    body: "ship it".to_string(),
                },
            ),
            env(
                6,
                Event::MailSent {
                    session: root,
                    from: "reviewer-1".to_string(),
                    to: MailEndpoint::Handle("reviewer-2".to_string()),
                    kind: MailKind::Message,
                    body: "psst".to_string(),
                },
            ),
        ]
    }

    #[test]
    fn channel_mail_folds_into_every_subscriber_inbox_and_survives_replay() {
        let root = SessionId::new();
        let log = build_log(root);

        // Apply incrementally.
        let mut live = Projection::default();
        for e in &log {
            live.apply(e);
        }

        // The channel post reached BOTH subscribers' inboxes...
        let inbox_of = |p: &Projection, handle: &str| -> Vec<String> {
            p.team
                .inboxes
                .get(handle)
                .map(|msgs| msgs.iter().map(|m| m.body.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(inbox_of(&live, "reviewer-1"), vec!["ship it".to_string()]);
        assert_eq!(
            inbox_of(&live, "reviewer-2"),
            vec!["ship it".to_string(), "psst".to_string()],
            "reviewer-2 sees the channel post AND the direct message, in order"
        );
        // ...and the channel keeps its own ordered log.
        let channel = live.team.channels.get("build").expect("channel exists");
        assert_eq!(channel.members.len(), 2);
        assert_eq!(channel.log.len(), 1);
        assert_eq!(channel.log[0].body, "ship it");
        // Roster bound both handles.
        assert_eq!(live.team.roster.len(), 2);

        // A fresh replay from the same log reconstructs identical team state.
        let replayed = Projection::from_events(&log);
        assert_eq!(
            replayed.team, live.team,
            "replay must reconstruct inboxes + channels + roster exactly"
        );
    }
}
