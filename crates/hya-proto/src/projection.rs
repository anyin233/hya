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
use crate::scope;

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
    /// Per-agent inbox keyed by **canonical path**, in delivery (seq) order. A
    /// direct message lands in the recipient's inbox; a channel message lands in
    /// every current subscriber's inbox.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inboxes: BTreeMap<String, Vec<MailMessage>>,
    /// Channels keyed by **unit-qualified key** (`main/lead-1#build`, no leading
    /// `#`): membership set + full message log. Two units may each own a channel
    /// of the same name; the qualifier is what keeps them separate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, ChannelProjection>,
    /// Live team roster keyed by **canonical path**.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roster: BTreeMap<String, RosterEntry>,
}

/// Why a `#channel` address could not be resolved to a unit-qualified key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChannelResolveError {
    /// The name is empty or carries a structural separator.
    InvalidName(String),
    /// `#announce` is reserved for the one-way announce path.
    Reserved,
    /// `#^name` was used by an agent that leads no unit, where it is meaningless
    /// because plain `#name` already addresses its home unit.
    CaretWithoutReports,
    /// The team root used `#^name`; it has no parent unit.
    NoHomeUnit,
}

impl std::fmt::Display for ChannelResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelResolveError::InvalidName(name) => write!(
                f,
                "`{name}` is not a usable channel name (it must be non-empty and \
                 free of `/`, `#`, and surrounding spaces)"
            ),
            ChannelResolveError::Reserved => write!(
                f,
                "`#{}` is reserved for one-way announcements; use the `announce` \
                 tool to post to your reports",
                scope::ANNOUNCE_CHANNEL
            ),
            ChannelResolveError::CaretWithoutReports => write!(
                f,
                "`#^name` addresses your parent's unit and is only meaningful for \
                 an agent that leads a unit of its own; use `#name`"
            ),
            ChannelResolveError::NoHomeUnit => write!(
                f,
                "the team root has no parent unit, so `#^name` has nothing to \
                 address"
            ),
        }
    }
}

impl TeamProjection {
    /// Resolve a handle as written on the wire to its canonical path.
    ///
    /// New emitters always write canonical paths, so this is the identity for
    /// any log written after scoping landed. The remaining branches exist purely
    /// to replay **pre-scoping** logs, which carry bare leaf names:
    ///
    /// 1. anything containing a `/` is already canonical;
    /// 2. anything that is already a roster key is canonical (this is what
    ///    matches the root, whose path `main` has no separator);
    /// 3. otherwise resolve the leaf against the roster, which is unambiguous in
    ///    a flat legacy team;
    /// 4. failing that, attach it to the root, matching how a legacy
    ///    `AgentRegistered` with no `parent` folds.
    ///
    /// Step 4 also covers ordering: a `MailSent` folded before the recipient's
    /// `AgentRegistered` still lands under the same key that registration will
    /// later produce, so replay does not depend on event order.
    ///
    /// Public because the reducer is not the only reader of a raw address: the
    /// resident supervisor resolves a live `MailSent`'s recipients against this
    /// same roster, and the two must agree or a delivered message would fail to
    /// wake its recipient.
    #[must_use]
    pub fn canonical_member(&self, raw: &str) -> String {
        if raw.contains(scope::PATH_SEPARATOR) || self.roster.contains_key(raw) {
            return raw.to_string();
        }
        let mut matches = self.roster.keys().filter(|key| scope::leaf(key) == raw);
        match (matches.next(), matches.next()) {
            // Exactly one roster entry carries this leaf.
            (Some(only), None) => only.clone(),
            // Unknown, or ambiguous across units: fall back to the legacy
            // interpretation rather than guessing which unit was meant.
            _ => scope::join_path(scope::ROOT_HANDLE, raw),
        }
    }

    /// Resolve a channel name as written on the wire to its unit-qualified key.
    ///
    /// New emitters write qualified keys (`main/lead-1#build`). A pre-scoping log
    /// carries a bare name, which belongs to the root's unit because a legacy
    /// team is one flat unit under `main`.
    #[must_use]
    pub fn canonical_channel(&self, raw: &str) -> String {
        if raw.contains(scope::CHANNEL_SEPARATOR) {
            return raw.to_string();
        }
        scope::qualify_channel(scope::ROOT_HANDLE, raw)
    }

    /// Whether `path` leads a unit — that is, whether any roster entry is its
    /// direct child.
    #[must_use]
    pub fn leads_a_unit(&self, path: &str) -> bool {
        self.roster
            .keys()
            .any(|key| scope::parent_path(key) == Some(path))
    }

    /// Resolve a mail address as a model wrote it — a relative leaf
    /// (`worker-1`) or a full canonical path (`main/lead-1/worker-1`) — to the
    /// canonical path of an agent `from` is allowed to address.
    ///
    /// `None` means the address is unusable from `from`: unknown, out of scope,
    /// or (defensively) ambiguous. All three collapse to one answer on purpose,
    /// so a sender cannot probe whether an out-of-scope agent exists.
    ///
    /// Ambiguity cannot arise while registration enforces leaf uniqueness within
    /// a unit, but refusing it here means a log that predates or violates that
    /// rule fails closed instead of silently picking a recipient.
    #[must_use]
    pub fn resolve_in_scope(&self, from: &str, raw: &str) -> Option<String> {
        let raw = raw.trim();
        let mut matches = self
            .roster
            .keys()
            .filter(|key| scope::in_scope(from, key))
            .filter(|key| key.as_str() == raw || scope::leaf(key) == raw);
        match (matches.next(), matches.next()) {
            (Some(only), None) => Some(only.clone()),
            _ => None,
        }
    }

    /// Resolve a channel address as a model wrote it (without the leading `#`)
    /// to a unit-qualified key.
    ///
    /// | Sender | `build` | `^build` |
    /// | --- | --- | --- |
    /// | leads a unit | that unit's `#build` | its home unit's `#build` |
    /// | leads nothing | its home unit's `#build` | error |
    ///
    /// A leader defaults to the unit it leads because that is the team it
    /// coordinates; `^` reaches sideways to its fellow leaders.
    pub fn resolve_channel(&self, from: &str, raw: &str) -> Result<String, ChannelResolveError> {
        let raw = raw.trim();
        let (name, want_home) = match raw.strip_prefix('^') {
            Some(name) => (name.trim(), true),
            None => (raw, false),
        };
        if !scope::is_valid_channel_name(name) {
            return Err(ChannelResolveError::InvalidName(name.to_string()));
        }
        if name == scope::ANNOUNCE_CHANNEL {
            return Err(ChannelResolveError::Reserved);
        }
        let leads = self.leads_a_unit(from);
        let unit = if want_home {
            if !leads {
                return Err(ChannelResolveError::CaretWithoutReports);
            }
            scope::home_unit(from).ok_or(ChannelResolveError::NoHomeUnit)?
        } else if leads {
            scope::led_unit(from)
        } else {
            scope::home_unit(from).ok_or(ChannelResolveError::NoHomeUnit)?
        };
        Ok(scope::qualify_channel(unit, name))
    }

    /// The roster rows `from` may see, bucketed by how each stands to it.
    #[must_use]
    pub fn scoped_roster(&self, from: &str) -> ScopedRoster {
        let mut scoped = ScopedRoster {
            self_path: from.to_string(),
            parent: None,
            peers: Vec::new(),
            reports: Vec::new(),
        };
        for (path, entry) in &self.roster {
            match scope::relation(from, path) {
                Some(scope::Relation::Parent) => scoped.parent = Some(entry.clone()),
                Some(scope::Relation::Peer) => scoped.peers.push(entry.clone()),
                Some(scope::Relation::Report) => scoped.reports.push(entry.clone()),
                Some(scope::Relation::Own) | None => {}
            }
        }
        scoped
    }

    /// The channels `from` may see: those owned by its home unit and, when it
    /// leads one, by the unit it leads. The reserved announce channels are
    /// excluded — they are not joinable and are not part of the channel surface.
    #[must_use]
    pub fn scoped_channels(&self, from: &str) -> Vec<(&str, &ChannelProjection)> {
        let home = scope::home_unit(from);
        let led = self.leads_a_unit(from).then(|| scope::led_unit(from));
        self.channels
            .iter()
            .filter(|(key, _)| !scope::is_announce_channel(key))
            .filter(|(key, _)| {
                let unit = scope::channel_unit(key);
                unit == home || unit == led
            })
            .map(|(key, channel)| (key.as_str(), channel))
            .collect()
    }
}

/// The roster as one agent sees it: itself, its parent, its same-parent peers,
/// and its direct reports. Nothing outside its unit appears.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopedRoster {
    /// The viewing agent's own canonical path.
    pub self_path: String,
    /// Its parent, absent only for the team root.
    pub parent: Option<RosterEntry>,
    /// Agents sharing its parent.
    pub peers: Vec<RosterEntry>,
    /// Agents it directly leads.
    pub reports: Vec<RosterEntry>,
}

impl ScopedRoster {
    /// Every visible row, regardless of relation.
    #[must_use]
    pub fn entries(&self) -> Vec<&RosterEntry> {
        self.parent
            .iter()
            .chain(self.peers.iter())
            .chain(self.reports.iter())
            .collect()
    }

    /// Whether the viewer can see nobody at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parent.is_none() && self.peers.is_empty() && self.reports.is_empty()
    }
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
    /// Stable **canonical path** (mail address and UI label), e.g.
    /// `main/lead-1/worker-2`.
    ///
    /// There is deliberately no separate `parent` field: the path already
    /// encodes it (`hya_proto::scope::parent_path`), and storing it twice would
    /// create a second source of truth that can disagree with this key.
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

/// The canonical path an `AgentRegistered` binds, derived from the event's own
/// fields so the reducer stays a pure fold (no store, no lineage query).
///
/// | Case | Path |
/// | --- | --- |
/// | `agent_session == session` (the team root registering itself) | `main` |
/// | `parent = Some(p)` | `{p}/{handle}` |
/// | `parent = None`, non-root — **pre-scoping logs only** | `main/{handle}` |
///
/// The root's canonical path is the fixed [`scope::ROOT_HANDLE`] rather than the
/// event's `handle`. Every root registration already emits `main`, and pinning it
/// keeps the third row meaningful: a legacy child can only be attached to a root
/// whose path is known without reading any other event.
fn canonical_registered_path(
    session: SessionId,
    agent_session: SessionId,
    handle: &str,
    parent: &Option<String>,
) -> String {
    if agent_session == session {
        return scope::ROOT_HANDLE.to_string();
    }
    match parent {
        Some(parent) => scope::join_path(parent, handle),
        None => scope::join_path(scope::ROOT_HANDLE, handle),
    }
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
                session,
                agent_session,
                handle,
                parent,
                agent_type,
                mode,
            } => {
                let path = canonical_registered_path(*session, *agent_session, handle, parent);
                // Re-registering an existing path preserves its live status /
                // current_task (the reducer keys the roster by canonical path)
                // while refreshing the binding + mode.
                let entry = self
                    .team
                    .roster
                    .entry(path.clone())
                    .or_insert_with(|| RosterEntry {
                        handle: path.clone(),
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
                let handle = &self.canonical_member(handle);
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
                let handle = &self.canonical_member(handle);
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
                let key = self.canonical_channel(channel);
                let member = self.canonical_member(member);
                self.team
                    .channels
                    .entry(key)
                    .or_default()
                    .members
                    .insert(member);
            }
            Event::ChannelLeft {
                channel, member, ..
            } => {
                let key = self.canonical_channel(channel);
                let member = self.canonical_member(member);
                if let Some(ch) = self.team.channels.get_mut(&key) {
                    ch.members.remove(&member);
                }
            }
            Event::MailSent {
                from,
                to,
                kind,
                body,
                ..
            } => {
                // Canonicalize both endpoints so EVERY handle in the projection
                // is a canonical path. A pre-scoping log carries bare leaves and
                // bare channel names; leaving them raw here would strand the
                // message under a key no roster entry matches.
                let to = match to {
                    MailEndpoint::Handle(handle) => {
                        MailEndpoint::Handle(self.canonical_member(handle))
                    }
                    MailEndpoint::Channel(channel) => {
                        MailEndpoint::Channel(self.canonical_channel(channel))
                    }
                };
                let message = MailMessage {
                    from: self.canonical_member(from),
                    to: to.clone(),
                    kind: *kind,
                    body: body.clone(),
                };
                match &to {
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
            | Event::ContextEvicted { .. }
            | Event::Unknown => {}
        }
    }

    fn canonical_member(&self, raw: &str) -> String {
        self.team.canonical_member(raw)
    }

    fn canonical_channel(&self, raw: &str) -> String {
        self.team.canonical_channel(raw)
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

    /// The full team event log for the invariant: two agents join `#build`, the
    /// root posts to it, plus one direct message between the two. Reused by both
    /// the fold test and the replay test so they are provably reconstructing the
    /// SAME state.
    ///
    /// Written in the **post-scoping** emitter shape: registrations carry a real
    /// `parent`, and every handle / channel on the wire is already canonical.
    /// Pre-scoping logs are covered separately by
    /// `tests/legacy_flat_mailbox.rs`.
    fn build_log(root: SessionId) -> Vec<Envelope> {
        let alice = SessionId::new();
        let bob = SessionId::new();
        vec![
            env(
                1,
                Event::AgentRegistered {
                    session: root,
                    agent_session: root,
                    handle: "main".to_string(),
                    parent: None,
                    agent_type: AgentName::new("build"),
                    mode: SubagentMode::Transient,
                },
            ),
            env(
                2,
                Event::AgentRegistered {
                    session: root,
                    agent_session: alice,
                    handle: "reviewer-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                3,
                Event::AgentRegistered {
                    session: root,
                    agent_session: bob,
                    handle: "reviewer-2".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                4,
                Event::ChannelJoined {
                    session: root,
                    channel: "main#build".to_string(),
                    member: "main/reviewer-1".to_string(),
                },
            ),
            env(
                5,
                Event::ChannelJoined {
                    session: root,
                    channel: "main#build".to_string(),
                    member: "main/reviewer-2".to_string(),
                },
            ),
            env(
                6,
                Event::MailSent {
                    session: root,
                    from: "main".to_string(),
                    to: MailEndpoint::Channel("main#build".to_string()),
                    kind: MailKind::Announcement,
                    body: "ship it".to_string(),
                },
            ),
            env(
                7,
                Event::MailSent {
                    session: root,
                    from: "main/reviewer-1".to_string(),
                    to: MailEndpoint::Handle("main/reviewer-2".to_string()),
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
        assert_eq!(
            inbox_of(&live, "main/reviewer-1"),
            vec!["ship it".to_string()]
        );
        assert_eq!(
            inbox_of(&live, "main/reviewer-2"),
            vec!["ship it".to_string(), "psst".to_string()],
            "reviewer-2 sees the channel post AND the direct message, in order"
        );
        // ...and the channel keeps its own ordered log, under its unit-qualified
        // key. The bare name is NOT a key: two units may each own a `#build`.
        assert!(!live.team.channels.contains_key("build"));
        let channel = live
            .team
            .channels
            .get("main#build")
            .expect("channel exists under its unit-qualified key");
        assert_eq!(channel.members.len(), 2);
        assert_eq!(channel.log.len(), 1);
        assert_eq!(channel.log[0].body, "ship it");
        // Roster bound the root plus both members, keyed by canonical path.
        assert_eq!(live.team.roster.len(), 3);
        assert!(live.team.roster.contains_key("main"));
        assert!(live.team.roster.contains_key("main/reviewer-1"));
        assert!(live.team.roster.contains_key("main/reviewer-2"));

        // A fresh replay from the same log reconstructs identical team state.
        let replayed = Projection::from_events(&log);
        assert_eq!(
            replayed.team, live.team,
            "replay must reconstruct inboxes + channels + roster exactly"
        );
    }

    /// Two sibling units each own a `#build`. Because the channel key is
    /// unit-qualified, a post in one unit must not touch the other's log or any
    /// of its members' inboxes (task 08-07, AC4).
    ///
    /// ```text
    /// main
    /// ├── lead-1 ── worker-1   #build (lead-1's)
    /// └── lead-2 ── worker-7   #build (lead-2's)
    /// ```
    #[test]
    fn same_channel_name_in_two_units_never_cross_talks() {
        let root = SessionId::new();
        let mut seq = 0;
        let mut log = Vec::new();
        let mut push = |event: Event, seq: &mut u64| {
            *seq += 1;
            log.push(env(*seq, event));
        };

        push(
            Event::AgentRegistered {
                session: root,
                agent_session: root,
                handle: "main".to_string(),
                parent: None,
                agent_type: AgentName::new("build"),
                mode: SubagentMode::Transient,
            },
            &mut seq,
        );
        for (leaf, parent) in [
            ("lead-1", "main"),
            ("lead-2", "main"),
            ("worker-1", "main/lead-1"),
            ("worker-7", "main/lead-2"),
        ] {
            push(
                Event::AgentRegistered {
                    session: root,
                    agent_session: SessionId::new(),
                    handle: leaf.to_string(),
                    parent: Some(parent.to_string()),
                    agent_type: AgentName::new("worker"),
                    mode: SubagentMode::Resident,
                },
                &mut seq,
            );
        }

        // Each unit's members join THEIR unit's #build.
        for (unit, member) in [
            ("main/lead-1", "main/lead-1/worker-1"),
            ("main/lead-2", "main/lead-2/worker-7"),
        ] {
            push(
                Event::ChannelJoined {
                    session: root,
                    channel: format!("{unit}#build"),
                    member: member.to_string(),
                },
                &mut seq,
            );
        }

        // lead-1 posts to its own #build only.
        push(
            Event::MailSent {
                session: root,
                from: "main/lead-1".to_string(),
                to: MailEndpoint::Channel("main/lead-1#build".to_string()),
                kind: MailKind::Message,
                body: "unit one only".to_string(),
            },
            &mut seq,
        );

        let projection = Projection::from_events(&log);

        // Two distinct channels share the name `build`.
        assert_eq!(projection.team.channels.len(), 2);
        let left = projection
            .team
            .channels
            .get("main/lead-1#build")
            .expect("lead-1 owns a #build");
        let right = projection
            .team
            .channels
            .get("main/lead-2#build")
            .expect("lead-2 owns a SEPARATE #build");

        assert_eq!(left.log.len(), 1, "the post landed in lead-1's channel");
        assert_eq!(left.log[0].body, "unit one only");
        assert!(right.log.is_empty(), "lead-2's #build must be untouched");

        // Only the posting unit's member received it.
        let inbox = |handle: &str| -> Vec<String> {
            projection
                .team
                .inboxes
                .get(handle)
                .map(|msgs| msgs.iter().map(|m| m.body.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(
            inbox("main/lead-1/worker-1"),
            vec!["unit one only".to_string()]
        );
        assert_eq!(
            inbox("main/lead-2/worker-7"),
            Vec::<String>::new(),
            "a sibling unit's worker must never see another unit's channel post"
        );
    }

    /// Build the standard two-unit org used by the resolution tests:
    ///
    /// ```text
    /// main
    /// ├── lead-1 ── worker-1, worker-2
    /// └── lead-2 ── worker-1        <- SAME leaf as lead-1's, different unit
    /// ```
    fn two_unit_team() -> TeamProjection {
        let root = SessionId::new();
        let mut seq = 0;
        let mut log = Vec::new();
        let mut push = |leaf: &str, parent: Option<&str>, seq: &mut u64| {
            *seq += 1;
            let agent_session = if parent.is_none() {
                root
            } else {
                SessionId::new()
            };
            log.push(env(
                *seq,
                Event::AgentRegistered {
                    session: root,
                    agent_session,
                    handle: leaf.to_string(),
                    parent: parent.map(str::to_string),
                    agent_type: AgentName::new("worker"),
                    mode: SubagentMode::Resident,
                },
            ));
        };
        push("main", None, &mut seq);
        push("lead-1", Some("main"), &mut seq);
        push("lead-2", Some("main"), &mut seq);
        push("worker-1", Some("main/lead-1"), &mut seq);
        push("worker-2", Some("main/lead-1"), &mut seq);
        push("worker-1", Some("main/lead-2"), &mut seq);
        Projection::from_events(&log).team
    }

    /// A relative leaf and the full path name the same agent, and both are
    /// refused for anyone outside the sender's unit (AC1, AC2).
    #[test]
    fn resolve_in_scope_accepts_leaf_and_path_but_only_within_the_unit() {
        let team = two_unit_team();
        let worker = "main/lead-1/worker-1";

        // Sibling, by leaf and by full path — same answer.
        assert_eq!(
            team.resolve_in_scope(worker, "worker-2").as_deref(),
            Some("main/lead-1/worker-2")
        );
        assert_eq!(
            team.resolve_in_scope(worker, "main/lead-1/worker-2")
                .as_deref(),
            Some("main/lead-1/worker-2")
        );
        // Parent.
        assert_eq!(
            team.resolve_in_scope(worker, "lead-1").as_deref(),
            Some("main/lead-1")
        );
        // Direct report, seen from the leader.
        assert_eq!(
            team.resolve_in_scope("main/lead-1", "worker-1").as_deref(),
            Some("main/lead-1/worker-1")
        );

        // Out of scope, however it is spelled.
        for raw in ["main", "lead-2", "main/lead-2", "main/lead-2/worker-1"] {
            assert_eq!(
                team.resolve_in_scope(worker, raw),
                None,
                "`{raw}` must not resolve from {worker}"
            );
        }
        // Unknown and out-of-scope are indistinguishable to the sender.
        assert_eq!(team.resolve_in_scope(worker, "nobody"), None);
    }

    /// The two units each hold a `worker-1`. A relative leaf must resolve to the
    /// sender's OWN unit, never leak across, and never resolve to itself.
    #[test]
    fn duplicate_leaf_in_another_unit_resolves_locally_only() {
        let team = two_unit_team();
        assert_eq!(
            team.resolve_in_scope("main/lead-1/worker-2", "worker-1")
                .as_deref(),
            Some("main/lead-1/worker-1"),
            "the sender's own unit wins"
        );
        assert_eq!(
            team.resolve_in_scope("main/lead-2", "worker-1").as_deref(),
            Some("main/lead-2/worker-1"),
            "lead-2 reaches its OWN worker-1"
        );
        assert_eq!(
            team.resolve_in_scope("main/lead-1/worker-1", "worker-1"),
            None,
            "an agent may not address itself"
        );
    }

    /// `#name` vs `#^name`, for a leader and for a leaf agent (AC5).
    #[test]
    fn channel_resolution_follows_leadership() {
        let team = two_unit_team();

        // A leader: bare name is its OWN unit, `^` reaches its home unit.
        assert_eq!(
            team.resolve_channel("main/lead-1", "build"),
            Ok("main/lead-1#build".to_string())
        );
        assert_eq!(
            team.resolve_channel("main/lead-1", "^build"),
            Ok("main#build".to_string())
        );

        // A leaf agent: bare name is its home unit; `^` is meaningless.
        assert_eq!(
            team.resolve_channel("main/lead-1/worker-1", "build"),
            Ok("main/lead-1#build".to_string())
        );
        assert_eq!(
            team.resolve_channel("main/lead-1/worker-1", "^build"),
            Err(ChannelResolveError::CaretWithoutReports)
        );

        // The root leads a unit but has no home unit above it.
        assert_eq!(
            team.resolve_channel("main", "build"),
            Ok("main#build".to_string())
        );
        assert_eq!(
            team.resolve_channel("main", "^build"),
            Err(ChannelResolveError::NoHomeUnit)
        );
    }

    #[test]
    fn announce_channel_cannot_be_addressed_as_an_ordinary_channel() {
        let team = two_unit_team();
        assert_eq!(
            team.resolve_channel("main/lead-1", "announce"),
            Err(ChannelResolveError::Reserved)
        );
        assert_eq!(
            team.resolve_channel("main/lead-1", "^announce"),
            Err(ChannelResolveError::Reserved)
        );
    }

    #[test]
    fn channel_names_carrying_separators_are_refused() {
        let team = two_unit_team();
        for bad in ["", "a/b", "a#b"] {
            assert!(
                matches!(
                    team.resolve_channel("main/lead-1", bad),
                    Err(ChannelResolveError::InvalidName(_))
                ),
                "`{bad}` must be refused"
            );
        }
    }

    /// The roster one agent sees, bucketed by relation, with nothing from
    /// another unit (AC7).
    #[test]
    fn scoped_roster_shows_only_the_unit() {
        let team = two_unit_team();

        let lead = team.scoped_roster("main/lead-1");
        assert_eq!(lead.self_path, "main/lead-1");
        assert_eq!(
            lead.parent.as_ref().map(|e| e.handle.as_str()),
            Some("main")
        );
        assert_eq!(
            lead.peers
                .iter()
                .map(|e| e.handle.as_str())
                .collect::<Vec<_>>(),
            vec!["main/lead-2"]
        );
        assert_eq!(
            lead.reports
                .iter()
                .map(|e| e.handle.as_str())
                .collect::<Vec<_>>(),
            vec!["main/lead-1/worker-1", "main/lead-1/worker-2"]
        );

        // A worker sees its leader and its one sibling — and nothing else.
        let worker = team.scoped_roster("main/lead-1/worker-1");
        assert_eq!(
            worker.parent.as_ref().map(|e| e.handle.as_str()),
            Some("main/lead-1")
        );
        assert_eq!(
            worker
                .peers
                .iter()
                .map(|e| e.handle.as_str())
                .collect::<Vec<_>>(),
            vec!["main/lead-1/worker-2"]
        );
        assert!(worker.reports.is_empty());
        assert_eq!(worker.entries().len(), 2, "6 agents exist; 2 are visible");

        // The root has no parent and no peers.
        let root = team.scoped_roster("main");
        assert!(root.parent.is_none());
        assert!(root.peers.is_empty());
        assert_eq!(root.reports.len(), 2);
    }

    /// The reducer builds a real tree from `parent`, and the resulting paths obey
    /// the scope rule the rest of the system enforces.
    #[test]
    fn nested_registration_folds_into_scoped_paths() {
        let root = SessionId::new();
        let log = vec![
            env(
                1,
                Event::AgentRegistered {
                    session: root,
                    agent_session: root,
                    handle: "main".to_string(),
                    parent: None,
                    agent_type: AgentName::new("build"),
                    mode: SubagentMode::Transient,
                },
            ),
            env(
                2,
                Event::AgentRegistered {
                    session: root,
                    agent_session: SessionId::new(),
                    handle: "lead-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("lead"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                3,
                Event::AgentRegistered {
                    session: root,
                    agent_session: SessionId::new(),
                    handle: "worker-1".to_string(),
                    parent: Some("main/lead-1".to_string()),
                    agent_type: AgentName::new("worker"),
                    mode: SubagentMode::Resident,
                },
            ),
        ];
        let projection = Projection::from_events(&log);

        let keys: Vec<&str> = projection.team.roster.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["main", "main/lead-1", "main/lead-1/worker-1"]);

        // The tree the fold produced is the tree the scope rule reads.
        assert!(crate::in_scope("main/lead-1/worker-1", "main/lead-1"));
        assert!(crate::in_scope("main/lead-1", "main"));
        assert!(
            !crate::in_scope("main/lead-1/worker-1", "main"),
            "skip-level must stay closed"
        );
    }
}
