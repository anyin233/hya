use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{Projection, is_zero};
use crate::event::{ArchiveReason, Event, ReportOutcome};
use crate::ids::{ActorEpoch, MemberId, SessionId, ToolCallId};
use crate::mail::{ChannelKind, MailEndpoint, MailKind};
use crate::message::{MemberRunStatus, RosterStatus, SubagentMode};
use crate::model::AgentName;
use crate::scope;

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

/// Team-scoped mailbox/channel state (ADR-0001), folded from the team-root log.
///
/// A replay of the root session's event log reconstructs this exactly: handles
/// are baked into `AgentRegistered`, channel membership into `ChannelCreated`,
/// and every `MailSent` is appended (in seq order) to the
/// recipient inboxes / channel log the reducer resolves at fold time. Kept on the
/// top-level [`Projection`] rather than [`super::SessionProjection`] because it belongs
/// to the whole team tree, not one session's transcript.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamProjection {
    /// Mail records in fold order, stored once regardless of recipient count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mail: Vec<MailMessage>,
    /// Per-agent inbox keyed by **canonical path**, in delivery (seq) order. A
    /// direct message lands in the recipient's inbox; a channel message lands in
    /// every current subscriber's inbox. Entries index `mail`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inboxes: BTreeMap<String, Vec<usize>>,
    /// Channels keyed by their minted id (`announce-{8}` / `DM-{8}`):
    /// membership set and full message log.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, ChannelProjection>,
    /// Live team roster keyed by **canonical path**.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roster: BTreeMap<String, RosterEntry>,
    /// Archived team history keyed by canonical path (ADR-0015): rows the
    /// user-side tree keeps after an agent left the live roster. Agent-facing
    /// tools never read this map; `search_agent` reads the store-derived index.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archived: BTreeMap<String, ArchivedEntry>,
}

impl RosterEntry {
    /// Whether the replayed member state accepts new mail.
    pub fn accepts_mail(&self) -> bool {
        !self.mode.is_resident()
            || !matches!(self.status, RosterStatus::Done | RosterStatus::Failed)
    }
}

/// One archived agent's history row (ADR-0015), folded from `AgentArchived`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArchivedEntry {
    /// Canonical handle the agent had while live.
    pub handle: String,
    /// Agent's own session id.
    pub session: SessionId,
    /// Declared agent type (from the removed roster row).
    #[serde(default)]
    pub agent_type: AgentName,
    /// Why the agent archived.
    pub reason: ArchiveReason,
}

impl TeamProjection {
    /// Incoming mail past a raw inbox cursor. Channel history includes the
    /// sender's own copy, which never counts as unread work for that handle.
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
    pub fn mail_recipient_is_live(&self, handle: &str) -> bool {
        self.roster
            .get(handle)
            .is_none_or(RosterEntry::accepts_mail)
    }

    /// Resolve an input handle (canonical path, leaf, or parent sentinel) to
    /// the caller's existing DM. Membership is the addressability boundary;
    /// archived peers and a lazily registered parent retain their DM.
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

/// One channel: its current subscribers plus the ordered log of everything posted
/// to it (independent of who was subscribed when).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelProjection {
    /// Group broadcast vs DM pair (ADR-0016).
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

#[cfg(feature = "lifecycle-bench")]
impl Clone for MailMessage {
    fn clone(&self) -> Self {
        crate::bench::mail_clone(self.body.len() as u64);
        Self {
            from: self.from.clone(),
            to: self.to.clone(),
            kind: self.kind,
            body: self.body.clone(),
        }
    }
}

/// A single delivered message as folded into an inbox / channel log.
#[cfg_attr(not(feature = "lifecycle-bench"), derive(Clone))]
#[derive(Debug, PartialEq, Serialize, Deserialize)]
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
    /// Inbox offset acknowledged within this episode; revival skips prior history.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resident_cursor: u64,
    /// Latest harness-observed activity time (Unix-epoch ms), folded from
    /// `AgentHeartbeat` as a max; `0` when no heartbeat was ever observed.
    /// Harness truth only — the agent itself cannot emit heartbeats.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub heartbeat_ms: u64,
    /// Present only after work starts and before it reaches a terminal/idle state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_work: Option<ResidentWorkProjection>,
}

/// Outstanding resident wake, retained across a user-cancelled Idle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidentWorkProjection {
    /// Actor epoch that started this work.
    pub epoch: ActorEpoch,
    /// Inbox length delivered to this wake; settling it acknowledges this boundary.
    pub inbox_through: u64,
}

pub(super) fn team_is_empty(team: &TeamProjection) -> bool {
    team.mail.is_empty()
        && team.inboxes.is_empty()
        && team.channels.is_empty()
        && team.roster.is_empty()
        && team.archived.is_empty()
}

impl Projection {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_member_spawned(
        &mut self,
        member: &MemberId,
        child: &Option<SessionId>,
        subagent_type: &AgentName,
        description: &str,
        depth: &u32,
        directive: &str,
        tool_call: &Option<ToolCallId>,
    ) {
        let entry = self.member_mut(*member);
        entry.child = *child;
        entry.subagent_type = subagent_type.clone();
        entry.description = description.to_owned();
        entry.depth = *depth;
        entry.status = MemberRunStatus::Spawning;
        // A resume re-emits MemberSpawned for the same member; keep the
        // original purpose rather than blanking it from a thinner re-emit.
        if !directive.is_empty() {
            entry.directive = directive.to_owned();
        }
        if tool_call.is_some() {
            entry.tool_call = *tool_call;
        }
    }

    pub(super) fn apply_member_status_changed(
        &mut self,
        member: &MemberId,
        status: &MemberRunStatus,
    ) {
        self.member_mut(*member).status = *status;
    }

    pub(super) fn apply_member_finished(
        &mut self,
        member: &MemberId,
        status: &MemberRunStatus,
        summary: &str,
        child: &Option<SessionId>,
    ) {
        let entry = self.member_mut(*member);
        entry.status = *status;
        entry.summary = summary.to_owned();
        if child.is_some() {
            entry.child = *child;
        }
    }

    pub(super) fn apply_agent_registered(
        &mut self,
        agent_session: &SessionId,
        handle: &str,
        parent: &Option<String>,
        agent_type: &AgentName,
        mode: &SubagentMode,
    ) {
        let path = match parent {
            Some(parent) => scope::join_path(parent, handle),
            None => handle.to_owned(),
        };
        // Re-registering an existing path preserves its live status /
        // current_task (the reducer keys the roster by canonical path)
        // while refreshing the binding + mode.
        // Archive removes unit broadcast membership; registering its
        // next episode restores the same unit without minting channels.
        if self.team.archived.contains_key(&path)
            && !self.team.roster.contains_key(&path)
            && let Some(parent) = parent
            && let Some(channel) = self.team.channels.values_mut().find(|channel| {
                channel.kind == ChannelKind::Group && channel.unit.as_ref() == Some(parent)
            })
        {
            channel.members.insert(path.clone());
        }
        let cursor = self
            .team
            .inboxes
            .get(&path)
            .map_or(0, |inbox| inbox.len() as u64);
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
                resident_cursor: cursor,
                heartbeat_ms: 0,
                resident_work: None,
            });
        entry.session = *agent_session;
        entry.agent_type = agent_type.clone();
        entry.mode = *mode;
    }

    pub(super) fn apply_agent_activity_changed(
        &mut self,
        handle: &str,
        status: &RosterStatus,
        current_task: &Option<String>,
    ) {
        if let Some(entry) = self.team.roster.get_mut(handle) {
            entry.status = *status;
            if current_task.is_some() {
                entry.current_task = current_task.clone();
            }
        }
    }

    pub(super) fn apply_resident_work_started(
        &mut self,
        actor_session: &SessionId,
        handle: &str,
        epoch: &ActorEpoch,
        inbox_through: &u64,
    ) {
        if let Some(entry) = self.team.roster.get_mut(handle)
            && entry.session == *actor_session
            && entry.mode.is_resident()
        {
            entry.status = RosterStatus::Busy;
            entry.resident_work = Some(ResidentWorkProjection {
                epoch: *epoch,
                inbox_through: *inbox_through,
            });
        }
    }

    pub(super) fn apply_agent_heartbeat(&mut self, handle: &str, heartbeat_ms: &u64) {
        // Max-fold: liveness only ever advances. A heartbeat observes an
        // existing row (the slot must be registered to be tracked) and
        // never materializes one, matching every other roster update.
        if let Some(entry) = self.team.roster.get_mut(handle) {
            entry.heartbeat_ms = entry.heartbeat_ms.max(*heartbeat_ms);
        }
    }

    pub(super) fn apply_channel_created(
        &mut self,
        channel: &str,
        kind: &ChannelKind,
        unit: &Option<String>,
        members: &[String],
    ) {
        let channel_state = self.team.channels.entry(channel.to_owned()).or_default();
        channel_state.kind = *kind;
        if unit.is_some() {
            channel_state.unit = unit.clone();
        }
        channel_state.members.extend(members.iter().cloned());
    }

    pub(super) fn apply_subagent_reported(
        &mut self,
        member: &MemberId,
        outcome: &ReportOutcome,
        report: &str,
    ) {
        let entry = self.member_mut(*member);
        entry.status = match outcome {
            ReportOutcome::Done => MemberRunStatus::Done,
            ReportOutcome::Failed => MemberRunStatus::Failed,
        };
        entry.summary = report.to_owned();
    }

    pub(super) fn apply_agent_archived(
        &mut self,
        handle: &str,
        child: &SessionId,
        reason: &ArchiveReason,
    ) {
        let handle = handle.to_owned();
        if let Some(entry) = self.team.roster.remove(&handle) {
            self.team.archived.insert(
                handle.to_owned(),
                ArchivedEntry {
                    handle: handle.to_owned(),
                    session: *child,
                    agent_type: entry.agent_type.clone(),
                    reason: *reason,
                },
            );
        }
        // Group membership ends at archive; DM channels keep both
        // members because the DM is the revival address (ADR-0016).
        for channel in self.team.channels.values_mut() {
            if channel.kind == ChannelKind::Group {
                channel.members.remove(&handle);
            }
        }
    }

    pub(super) fn apply_mail_consumed(&mut self, handle: &str, through: &u64, e: &Event) {
        if let Some(entry) = self.team.roster.get_mut(handle) {
            if let Event::ResidentWorkFinished { epoch, .. } = e {
                if entry.resident_work.is_none_or(|work| work.epoch != *epoch) {
                    return;
                }
                entry.resident_work = None;
            }
            entry.resident_cursor = entry.resident_cursor.max(*through);
        }
    }

    pub(super) fn apply_agent_restarted(&mut self, handle: &str) {
        let handle = handle.to_owned();
        self.team.archived.remove(&handle);
    }

    pub(super) fn apply_mail_sent(
        &mut self,
        from: &str,
        to: &MailEndpoint,
        kind: &MailKind,
        body: &str,
    ) {
        let message = MailMessage {
            from: from.to_owned(),
            to: to.clone(),
            kind: *kind,
            body: body.to_owned(),
        };
        let index = self.team.mail.len();
        self.team.mail.push(message);
        match to {
            // The store admits handle targets only for harness notices.
            MailEndpoint::Handle(handle) => {
                self.team
                    .inboxes
                    .entry(handle.clone())
                    .or_default()
                    .push(index);
            }
            MailEndpoint::Channel(channel) => {
                let channel_state = self.team.channels.entry(channel.clone()).or_default();
                channel_state.log.push(index);
                // Fan out to every CURRENT subscriber. Snapshot the member
                // set first so the inbox borrow does not alias the channel.
                let members: Vec<String> = channel_state.members.iter().cloned().collect();
                for member in members {
                    if !self.team.mail_recipient_is_live(&member) {
                        continue;
                    }
                    self.team.inboxes.entry(member).or_default().push(index);
                }
            }
        }
    }
}
