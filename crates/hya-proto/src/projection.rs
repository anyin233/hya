//! Shared idempotent reducer: fold an event log into a session view. Used by the
//! store (read path) and the client (SSE reconnect); idempotent by `EventSeq` so
//! re-delivered events are no-ops.

mod helpers;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use self::helpers::{find_part, push_part, tool_input, upsert_tool};
use crate::event::{Envelope, Event};
use crate::ids::{MemberId, MessageId, PartId, SessionId, ToolCallId};
use crate::mail::{MailEndpoint, MailKind};
use crate::message::{FinishReason, MemberRunStatus, Role, TokenUsage, ToolPartState};
use crate::model::{AgentName, ModelRef, ToolName};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionProjection {
    pub id: Option<SessionId>,
    pub parent: Option<SessionId>,
    pub agent: Option<AgentName>,
    pub model: Option<ModelRef>,
    pub workdir: Option<String>,
    pub title: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub permission: Option<Vec<serde_json::Value>>,
    pub archived: Option<serde_json::Number>,
    pub share: Option<String>,
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
    pub member: MemberId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<SessionId>,
    pub subagent_type: AgentName,
    pub description: String,
    pub depth: u32,
    pub status: MemberRunStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageProjection {
    pub id: MessageId,
    pub role: Role,
    pub finish: Option<FinishReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<serde_json::Value>,
    pub parts: Vec<PartProjection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PartProjection {
    Text {
        id: PartId,
        text: String,
    },
    Reasoning {
        id: PartId,
        text: String,
    },
    Tool {
        id: PartId,
        call: ToolCallId,
        name: ToolName,
        state: ToolPartState,
    },
}

impl PartProjection {
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
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub members: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log: Vec<MailMessage>,
}

/// A single delivered message as folded into an inbox / channel log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MailMessage {
    pub from: String,
    pub to: MailEndpoint,
    #[serde(default)]
    pub kind: MailKind,
    pub body: String,
}

/// A live team member: its handle, own session, and declared agent type. Status
/// and current-task enrichment arrive with the resident lifecycle (Phase 4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RosterEntry {
    pub handle: String,
    pub session: SessionId,
    #[serde(default)]
    pub agent_type: AgentName,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    pub session: SessionProjection,
    /// Team-scoped mailbox/channel/roster state. Empty on sessions that are not a
    /// team root / carry no mail events.
    #[serde(default, skip_serializing_if = "team_is_empty")]
    pub team: TeamProjection,
    pub last_seq: u64,
}

fn team_is_empty(team: &TeamProjection) -> bool {
    team.inboxes.is_empty() && team.channels.is_empty() && team.roster.is_empty()
}

impl Projection {
    #[must_use]
    pub fn from_events(envs: &[Envelope]) -> Self {
        let mut p = Self::default();
        for e in envs {
            p.apply(e);
        }
        p
    }

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
                        finish: None,
                        tokens: None,
                        files: Vec::new(),
                        agents: Vec::new(),
                        parts: Vec::new(),
                    });
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
                ..
            } => {
                let entry = self.member_mut(*member);
                entry.child = *child;
                entry.subagent_type = subagent_type.clone();
                entry.description = description.clone();
                entry.depth = *depth;
                entry.status = MemberRunStatus::Spawning;
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
                ..
            } => {
                self.team.roster.insert(
                    handle.clone(),
                    RosterEntry {
                        handle: handle.clone(),
                        session: *agent_session,
                        agent_type: agent_type.clone(),
                    },
                );
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
            | Event::ReasoningEnd { .. }
            | Event::SessionStatus { .. }
            | Event::ToolInputDelta { .. }
            | Event::CommandExecuted { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::Error { .. }
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
                },
            ),
            env(
                2,
                Event::AgentRegistered {
                    session: root,
                    agent_session: bob,
                    handle: "reviewer-2".to_string(),
                    agent_type: AgentName::new("reviewer"),
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
