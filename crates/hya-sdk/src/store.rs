//! The live message/part store (parity with `packages/tui/src/context/sync.tsx`).
//!
//! The visible session timeline is built from the top-level `message.updated` /
//! `message.part.updated` / `message.part.delta` events — NOT the `session.next.*`
//! turn-stream (that is the durable prompt-admission inbox, parked in `reducer.rs`).
//! `sync` envelopes are event-sourced replay duplicates and are ignored here.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use crate::team::TeamProjection;
use crate::types::{GlobalEvent, Message, Part, Session};
use crate::workflow::{
    WorkflowMemberProjection, WorkflowProjection, WorkflowRunProjection, WorkflowStageProjection,
    WorkflowStageStatus,
};

const MESSAGE_CAP: usize = 100;

/// A message part plus the keys the store sorts/indexes on (lifted out of the JSON
/// so `binary_search` never pays a map lookup per compare).
#[derive(Debug, Clone)]
pub struct StoredPart {
    /// Part id from the wire.
    pub id: String,
    /// Owning message id (`messageID` on the wire).
    pub message_id: String,
    /// Decoded part payload.
    pub inner: Part,
}

impl StoredPart {
    /// Decode a part JSON object that carries `id` and `messageID`.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id")?.as_str()?.to_string();
        let message_id = value.get("messageID")?.as_str()?.to_string();
        let inner = serde_json::from_value::<Part>(value.clone()).ok()?;
        Some(Self {
            id,
            message_id,
            inner,
        })
    }
}

/// A spawned subagent as projected for its parent session's live timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberProjection {
    /// Stable member id within the parent session.
    pub member: String,
    /// Child session id when the backend reports one.
    pub child: Option<String>,
    /// Agent type requested for the subagent.
    pub subagent_type: String,
    /// Short task description supplied at spawn time.
    pub description: String,
    /// Spawn depth in the subagent tree.
    pub depth: u32,
    /// Wire lifecycle status: spawning, running, done, failed, or cancelled.
    pub status: String,
    /// Terminal summary reported by member_finished.
    pub summary: String,
}
/// A Workflow member reference joined to its canonical Session member row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMemberActivity {
    /// Workflow Stage role and activation iteration.
    pub reference: WorkflowMemberProjection,
    /// Canonical member lifecycle row from [`MessageStore::members`].
    pub member: MemberProjection,
}

/// One Workflow Stage plus the canonical members linked to that Stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStageActivity {
    /// Replay-derived Stage plan and status.
    pub stage: WorkflowStageProjection,
    /// Members joined by stable member id; unrelated Session members are absent.
    pub members: Vec<WorkflowMemberActivity>,
}

/// Server-derived activity for the current Workflow state.
///
/// The activity is rebuilt from `Session.workflow` and the existing canonical
/// `MessageStore::members` projection. It is not stored or folded separately,
/// so unrelated run-tree members and duplicate Workflow references cannot
/// inflate the displayed counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowActivity {
    /// Selected Workflow identity, if one has been persisted.
    pub selection: Option<crate::workflow::WorkflowIdentity>,
    /// Current or terminal Workflow run, if one has been admitted.
    pub run: Option<WorkflowRunProjection>,
    /// Stages in the declaration order supplied by the server.
    pub stages: Vec<WorkflowStageActivity>,
    /// Number of unique referenced members whose lifecycle is active.
    pub active_agents: usize,
    /// Number of unique referenced members present in the canonical projection.
    pub total_agents: usize,
    /// Number of Stages with `completed` status.
    pub completed_stages: usize,
    /// Total number of Stages in the current run.
    pub total_stages: usize,
    /// Current graph level, preferring the first running/pending Stage.
    pub graph_level: Option<usize>,
    /// Running Stage ids in declaration order.
    pub running_stage_ids: Vec<String>,
    /// First running Stage, or first pending Stage before execution begins.
    pub current_stage: Option<WorkflowStageActivity>,
    /// First failed Stage, when the run contains one.
    pub failed_stage: Option<WorkflowStageActivity>,
}

impl WorkflowActivity {
    /// Derive activity by joining Workflow member references to canonical rows.
    #[must_use]
    pub fn from_projection(workflow: &WorkflowProjection, members: &[MemberProjection]) -> Self {
        let Some(run) = workflow.run.as_ref() else {
            return Self {
                selection: workflow.selection.clone(),
                run: None,
                stages: Vec::new(),
                active_agents: 0,
                total_agents: 0,
                completed_stages: 0,
                total_stages: 0,
                graph_level: None,
                running_stage_ids: Vec::new(),
                current_stage: None,
                failed_stage: None,
            };
        };

        let canonical: BTreeMap<&str, &MemberProjection> = members
            .iter()
            .map(|member| (member.member.as_str(), member))
            .collect();
        let mut unique_members: BTreeMap<String, &MemberProjection> = BTreeMap::new();
        let mut stages = Vec::with_capacity(run.stages.len());
        let mut running_stage_ids = Vec::new();
        let mut current_stage: Option<WorkflowStageActivity> = None;
        let mut failed_stage: Option<WorkflowStageActivity> = None;
        let mut graph_level = None;
        let mut completed_stages = 0;

        for stage in &run.stages {
            if stage.status == WorkflowStageStatus::Completed {
                completed_stages += 1;
            }
            let mut joined = Vec::new();
            for reference in &stage.members {
                let Some(member) = canonical.get(reference.member.as_str()) else {
                    continue;
                };
                if joined
                    .iter()
                    .any(|entry: &WorkflowMemberActivity| entry.reference == *reference)
                {
                    continue;
                }
                unique_members
                    .entry(reference.member.clone())
                    .or_insert(*member);
                joined.push(WorkflowMemberActivity {
                    reference: reference.clone(),
                    member: (*member).clone(),
                });
            }
            let activity = WorkflowStageActivity {
                stage: stage.clone(),
                members: joined,
            };
            match stage.status {
                WorkflowStageStatus::Running => {
                    let first_running = running_stage_ids.is_empty();
                    running_stage_ids.push(stage.plan.id.clone());
                    graph_level = if first_running {
                        Some(stage.plan.level)
                    } else {
                        Some(
                            graph_level
                                .map_or(stage.plan.level, |level| level.min(stage.plan.level)),
                        )
                    };
                    if current_stage
                        .as_ref()
                        .is_none_or(|current| current.stage.status != WorkflowStageStatus::Running)
                    {
                        current_stage = Some(activity.clone());
                    }
                }
                WorkflowStageStatus::Pending if current_stage.is_none() => {
                    if running_stage_ids.is_empty() {
                        graph_level = Some(stage.plan.level);
                    }
                    current_stage = Some(activity.clone());
                }
                WorkflowStageStatus::Failed if failed_stage.is_none() => {
                    failed_stage = Some(activity.clone());
                }
                _ => {}
            }
            stages.push(activity);
        }

        let active_agents = unique_members
            .values()
            .filter(|member| matches!(member.status.as_str(), "spawning" | "running"))
            .count();
        Self {
            selection: workflow.selection.clone(),
            run: Some(run.clone()),
            stages,
            active_agents,
            total_agents: unique_members.len(),
            completed_stages,
            total_stages: run.stages.len(),
            graph_level,
            running_stage_ids,
            current_stage,
            failed_stage,
        }
    }
}

/// Live conversation store: `sessionID -> messages` and `messageID -> parts`, each kept
/// id-sorted. Backend IDs are ULID-like (time-prefixed), so lexical order is chronological.
#[derive(Debug, Default)]
pub struct MessageStore {
    /// Session id → id-sorted messages.
    pub messages: HashMap<String, Vec<Message>>,
    /// Message id → id-sorted parts.
    pub parts: HashMap<String, Vec<StoredPart>>,
    /// Session id → latest session info.
    pub sessions: HashMap<String, Session>,
    session_working: HashMap<String, bool>,
    /// Session id → todo JSON rows.
    pub todos: HashMap<String, Vec<Value>>,
    /// Session id → diff JSON rows.
    pub diffs: HashMap<String, Vec<Value>>,
    /// Session id → pending permission request JSON.
    pub permissions: HashMap<String, Vec<Value>>,
    /// Session id → pending question request JSON.
    pub questions: HashMap<String, Vec<Value>>,
    /// Subagents spawned by parent session, folded from member lifecycle events.
    pub members: HashMap<String, Vec<MemberProjection>>,
    /// Per-team mailbox/channel/roster read-models keyed by team-root session id.
    pub teams: HashMap<String, TeamProjection>,
    /// Backward-compatible single-team aggregate; new TUI surfaces use [`Self::team_for`].
    pub team: TeamProjection,
}

impl MessageStore {
    /// Apply one decoded SSE event. Returns `true` if it mutated the store.
    pub fn apply_event(&mut self, event: &GlobalEvent) -> bool {
        let props = &event.payload.properties;
        match event.payload.kind.as_str() {
            "message.updated" => match props.get("info").cloned() {
                Some(info) => match serde_json::from_value::<Message>(info) {
                    Ok(message) => {
                        self.upsert_message(message);
                        true
                    }
                    Err(_) => false,
                },
                None => false,
            },
            "message.part.updated" => match props.get("part").and_then(StoredPart::from_value) {
                Some(part) => {
                    self.upsert_part(part);
                    true
                }
                None => false,
            },
            "message.part.delta" => {
                let Some(message_id) = str_field(props, "messageID") else {
                    return false;
                };
                let Some(part_id) = str_field(props, "partID") else {
                    return false;
                };
                let field = str_field(props, "field").unwrap_or("text");
                let delta = str_field(props, "delta").unwrap_or_default();
                self.apply_delta(message_id, part_id, field, delta)
            }
            "message.removed" => {
                let Some(session_id) = str_field(props, "sessionID") else {
                    return false;
                };
                let Some(message_id) = str_field(props, "messageID") else {
                    return false;
                };
                self.remove_message(session_id, message_id);
                true
            }
            "message.part.removed" => {
                let Some(message_id) = str_field(props, "messageID") else {
                    return false;
                };
                let Some(part_id) = str_field(props, "partID") else {
                    return false;
                };
                self.remove_part(message_id, part_id);
                true
            }
            "session.created" | "session.updated" => match props.get("info").cloned() {
                Some(info) => match serde_json::from_value::<Session>(info) {
                    Ok(session) => {
                        self.sessions.insert(session.id.clone(), session);
                        true
                    }
                    Err(_) => false,
                },
                None => false,
            },
            "session.status" => match (
                str_field(props, "sessionID"),
                props
                    .get("status")
                    .and_then(|status| str_field(status, "type")),
            ) {
                (Some(session_id), Some(status)) => {
                    self.session_working
                        .insert(session_id.to_owned(), status != "idle");
                    true
                }
                _ => false,
            },
            "todo.updated" => match (str_field(props, "sessionID"), props.get("todos")) {
                (Some(session_id), Some(Value::Array(todos))) => {
                    self.todos.insert(session_id.to_string(), todos.clone());
                    true
                }
                _ => false,
            },
            "session.diff" => match (str_field(props, "sessionID"), props.get("diff")) {
                (Some(session_id), Some(Value::Array(diff))) => {
                    self.diffs.insert(session_id.to_string(), diff.clone());
                    true
                }
                _ => false,
            },
            "permission.asked" => match str_field(props, "sessionID") {
                Some(session_id) => {
                    self.upsert_permission(session_id.to_string(), props.clone());
                    true
                }
                None => false,
            },
            "permission.replied" => {
                match (str_field(props, "sessionID"), str_field(props, "requestID")) {
                    (Some(session_id), Some(request_id)) => {
                        self.remove_permission(session_id, request_id);
                        true
                    }
                    _ => false,
                }
            }
            "question.asked" => match str_field(props, "sessionID") {
                Some(session_id) => {
                    self.upsert_question(session_id.to_string(), props.clone());
                    true
                }
                None => false,
            },
            "question.replied" | "question.rejected" => {
                match (str_field(props, "sessionID"), str_field(props, "requestID")) {
                    (Some(session_id), Some(request_id)) => {
                        self.remove_question(session_id, request_id);
                        true
                    }
                    _ => false,
                }
            }
            // Team and member lifecycle events arrive wrapped in a `hya.envelope`
            // whose `properties` is the raw backend envelope. Fold the inner event
            // into the small frontend read-models the TUI renders from.
            "hya.envelope" => match props.get("event") {
                Some(event) => {
                    let member_changed = self.apply_member_event(event);
                    let team_changed = self.apply_team_event(event);
                    member_changed || team_changed
                }
                None => false,
            },
            _ => false,
        }
    }

    fn apply_team_event(&mut self, event: &Value) -> bool {
        let scoped = str_field(event, "session")
            .map(|session| {
                self.teams
                    .entry(session.to_owned())
                    .or_default()
                    .apply_event(event)
            })
            .unwrap_or(false);
        self.team.apply_event(event) || scoped
    }

    /// Returns the live Team projection keyed by its root session id.
    ///
    /// Child routes should call `team_root_for` first when they need the Team
    /// that owns a child session, because Team events are not keyed by child id.
    #[must_use]
    pub fn team_for(&self, root_session: &str) -> Option<&TeamProjection> {
        self.teams.get(root_session)
    }

    /// Returns the topmost team session for `session_id`.
    ///
    /// Team events are keyed by the root session. Explicit parent ancestry stays
    /// authoritative even when the parent is not cached. Without ancestry or an
    /// owned Team, exactly one roster owner is used; zero or multiple owners keep
    /// the input id as the safest key.
    #[must_use]
    pub fn team_root_for<'a>(&'a self, session_id: &'a str) -> &'a str {
        let mut current = session_id;
        for _ in 0..=self.sessions.len() {
            let Some(parent) = self
                .sessions
                .get(current)
                .and_then(|session| session.parent_id.as_deref())
                .filter(|parent| !parent.is_empty())
            else {
                if current != session_id || self.teams.contains_key(session_id) {
                    return current;
                }
                let mut owners = self.teams.iter().filter_map(|(root, team)| {
                    team.roster
                        .values()
                        .any(|entry| entry.session == session_id)
                        .then_some(root.as_str())
                });
                let Some(owner) = owners.next() else {
                    return session_id;
                };
                return if owners.next().is_none() {
                    owner
                } else {
                    session_id
                };
            };
            current = parent;
        }
        session_id
    }

    fn apply_member_event(&mut self, event: &Value) -> bool {
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return false;
        };
        match kind {
            "member_spawned" => {
                let (Some(session), Some(member)) =
                    (str_field(event, "session"), str_field(event, "member"))
                else {
                    return false;
                };
                let child = str_field(event, "child").map(str::to_owned);
                let subagent_type = str_field(event, "subagent_type")
                    .unwrap_or_default()
                    .to_owned();
                let description = str_field(event, "description")
                    .unwrap_or_default()
                    .to_owned();
                let depth = event
                    .get("depth")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32;
                let entry = self.member_mut(session, member);
                entry.child = child;
                entry.subagent_type = subagent_type;
                entry.description = description;
                entry.depth = depth;
                entry.status = "spawning".to_owned();
                true
            }
            "member_status_changed" => {
                let (Some(session), Some(member), Some(status)) = (
                    str_field(event, "session"),
                    str_field(event, "member"),
                    str_field(event, "status"),
                ) else {
                    return false;
                };
                self.member_mut(session, member).status = status.to_owned();
                true
            }
            "member_finished" => {
                let (Some(session), Some(member), Some(status)) = (
                    str_field(event, "session"),
                    str_field(event, "member"),
                    str_field(event, "status"),
                ) else {
                    return false;
                };
                let summary = str_field(event, "summary").unwrap_or_default().to_owned();
                let child = str_field(event, "child").map(str::to_owned);
                let entry = self.member_mut(session, member);
                entry.status = status.to_owned();
                entry.summary = summary;
                if child.is_some() {
                    entry.child = child;
                }
                true
            }
            _ => false,
        }
    }

    fn member_mut(&mut self, session: &str, member: &str) -> &mut MemberProjection {
        let list = self.members.entry(session.to_owned()).or_default();
        if let Some(index) = list.iter().position(|entry| entry.member == member) {
            return &mut list[index];
        }
        list.push(MemberProjection {
            member: member.to_owned(),
            child: None,
            subagent_type: String::new(),
            description: String::new(),
            depth: 0,
            status: "spawning".to_owned(),
            summary: String::new(),
        });
        let last = list.len() - 1;
        &mut list[last]
    }

    /// Todo list for `session_id`, or empty slice.
    #[must_use]
    pub fn todos(&self, session_id: &str) -> &[Value] {
        self.todos.get(session_id).map_or(&[][..], Vec::as_slice)
    }

    /// Diff list for `session_id`, or empty slice.
    #[must_use]
    pub fn diffs(&self, session_id: &str) -> &[Value] {
        self.diffs.get(session_id).map_or(&[][..], Vec::as_slice)
    }

    /// Pending permission requests for `session_id`, or empty slice.
    #[must_use]
    pub fn permissions(&self, session_id: &str) -> &[Value] {
        self.permissions
            .get(session_id)
            .map_or(&[][..], Vec::as_slice)
    }

    /// Pending question requests for `session_id`, or empty slice.
    #[must_use]
    pub fn questions(&self, session_id: &str) -> &[Value] {
        self.questions
            .get(session_id)
            .map_or(&[][..], Vec::as_slice)
    }

    fn upsert_permission(&mut self, session_id: String, request: Value) {
        let id = request.get("id").and_then(Value::as_str).map(str::to_owned);
        let list = self.permissions.entry(session_id).or_default();
        let existing = id.and_then(|id| {
            list.iter()
                .position(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
        });
        match existing {
            Some(index) => list[index] = request,
            None => list.push(request),
        }
    }

    fn remove_permission(&mut self, session_id: &str, request_id: &str) {
        if let Some(list) = self.permissions.get_mut(session_id) {
            list.retain(|r| r.get("id").and_then(Value::as_str) != Some(request_id));
            if list.is_empty() {
                self.permissions.remove(session_id);
            }
        }
    }

    fn upsert_question(&mut self, session_id: String, request: Value) {
        let id = request.get("id").and_then(Value::as_str).map(str::to_owned);
        let list = self.questions.entry(session_id).or_default();
        let existing = id.and_then(|id| {
            list.iter()
                .position(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
        });
        match existing {
            Some(index) => list[index] = request,
            None => list.push(request),
        }
    }

    fn remove_question(&mut self, session_id: &str, request_id: &str) {
        if let Some(list) = self.questions.get_mut(session_id) {
            list.retain(|r| r.get("id").and_then(Value::as_str) != Some(request_id));
            if list.is_empty() {
                self.questions.remove(session_id);
            }
        }
    }

    /// Latest session info for `session_id`, if known.
    #[must_use]
    pub fn session(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id)
    }
    /// Derive current Workflow activity by joining referenced members to the
    /// canonical member projection for `session_id`.
    ///
    /// Returns `None` when the Session or its Workflow state is not hydrated.
    /// Only member ids referenced by the newest Workflow run are included;
    /// unrelated child members are intentionally excluded.
    #[must_use]
    pub fn workflow_activity(&self, session_id: &str) -> Option<WorkflowActivity> {
        let workflow = self.sessions.get(session_id)?.workflow.as_ref()?;
        Some(WorkflowActivity::from_projection(
            workflow,
            self.members.get(session_id).map_or(&[][..], Vec::as_slice),
        ))
    }

    /// Child sessions whose `parent_id` is `parent_id`, sorted by id.
    #[must_use]
    pub fn child_sessions(&self, parent_id: &str) -> Vec<&Session> {
        let mut children: Vec<&Session> = self
            .sessions
            .values()
            .filter(|session| session.parent_id.as_deref() == Some(parent_id))
            .collect();
        children.sort_by(|left, right| left.id.cmp(&right.id));
        children
    }

    /// Whether the session is treated as busy (explicit flag or incomplete last message).
    #[must_use]
    pub fn is_working(&self, session_id: &str) -> bool {
        if let Some(working) = self.session_working.get(session_id) {
            return *working;
        }
        let Some(last) = self.messages.get(session_id).and_then(|list| list.last()) else {
            return false;
        };
        match last.role.as_deref() {
            Some("user") => true,
            Some("assistant") => last.time.completed.is_none(),
            _ => false,
        }
    }

    /// Insert or replace a message, keeping the session list id-sorted and capped.
    pub fn upsert_message(&mut self, message: Message) {
        let Some(session_id) = message.session_id.clone() else {
            return;
        };
        let list = self.messages.entry(session_id.clone()).or_default();
        match list.binary_search_by(|m| m.id.as_str().cmp(&message.id)) {
            Ok(index) => list[index] = message,
            Err(index) => list.insert(index, message),
        }
        self.enforce_cap(&session_id);
    }

    /// Insert or replace a part under its message id, id-sorted.
    pub fn upsert_part(&mut self, part: StoredPart) {
        let list = self.parts.entry(part.message_id.clone()).or_default();
        match list.binary_search_by(|p| p.id.as_str().cmp(&part.id)) {
            Ok(index) => list[index] = part,
            Err(index) => list.insert(index, part),
        }
    }

    fn apply_delta(&mut self, message_id: &str, part_id: &str, field: &str, delta: &str) -> bool {
        let Some(list) = self.parts.get_mut(message_id) else {
            return false;
        };
        let Ok(index) = list.binary_search_by(|p| p.id.as_str().cmp(part_id)) else {
            return false;
        };
        apply_field_delta(&mut list[index].inner, field, delta);
        true
    }

    fn remove_message(&mut self, session_id: &str, message_id: &str) {
        if let Some(list) = self.messages.get_mut(session_id) {
            if let Ok(index) = list.binary_search_by(|m| m.id.as_str().cmp(message_id)) {
                list.remove(index);
            }
        }
        self.parts.remove(message_id);
    }

    fn remove_part(&mut self, message_id: &str, part_id: &str) {
        if let Some(list) = self.parts.get_mut(message_id) {
            if let Ok(index) = list.binary_search_by(|p| p.id.as_str().cmp(part_id)) {
                list.remove(index);
            }
        }
    }

    fn enforce_cap(&mut self, session_id: &str) {
        let mut dropped = Vec::new();
        if let Some(list) = self.messages.get_mut(session_id) {
            while list.len() > MESSAGE_CAP {
                dropped.push(list.remove(0).id);
            }
        }
        for message_id in dropped {
            self.parts.remove(&message_id);
        }
    }
}

fn str_field<'a>(props: &'a Value, key: &str) -> Option<&'a str> {
    props.get(key).and_then(Value::as_str)
}

fn apply_field_delta(part: &mut Part, field: &str, delta: &str) {
    // Parity: the TS reducer does `part[field] += delta`. text/reasoning stream into `.text`;
    // tool parts stream into other fields (input/output) which we stash in `rest` for the
    // W6 tool renderers. Unknown (part, field) pairs are dropped.
    match part {
        Part::Text { text, .. } | Part::Reasoning { text, .. } if field == "text" => {
            text.push_str(delta);
        }
        Part::Tool(tool) => stash_delta(&mut tool.rest, field, delta),
        _ => {}
    }
}

fn stash_delta(rest: &mut Map<String, Value>, field: &str, delta: &str) {
    if let Value::String(existing) = rest
        .entry(field.to_string())
        .or_insert_with(|| Value::String(String::new()))
    {
        existing.push_str(delta);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn message(id: &str, session_id: &str, role: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "id": id, "sessionID": session_id, "role": role, "time": {"created": 1}
        }))
        .unwrap()
    }

    fn text_part(id: &str, message_id: &str, text: &str) -> StoredPart {
        StoredPart::from_value(&serde_json::json!({
            "id": id, "messageID": message_id, "type": "text", "text": text
        }))
        .unwrap()
    }

    #[test]
    fn message_updated_upserts_by_id_in_order() {
        let mut store = MessageStore::default();
        store.upsert_message(message("msg_2", "ses_1", "assistant"));
        store.upsert_message(message("msg_1", "ses_1", "user"));
        let list = &store.messages["ses_1"];
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "msg_1");
        assert_eq!(list[1].id, "msg_2");
        store.upsert_message(message("msg_1", "ses_1", "user"));
        assert_eq!(store.messages["ses_1"].len(), 2, "re-upsert dedupes by id");
    }

    #[test]
    fn child_sessions_returns_children_sorted_by_id() {
        let event = |info: serde_json::Value| -> GlobalEvent {
            serde_json::from_value(serde_json::json!({
                "payload": { "type": "session.created", "properties": { "info": info } }
            }))
            .unwrap()
        };
        let mut store = MessageStore::default();
        store.apply_event(&event(serde_json::json!({ "id": "ses_parent" })));
        store.apply_event(&event(
            serde_json::json!({ "id": "ses_child_b", "parentID": "ses_parent" }),
        ));
        store.apply_event(&event(
            serde_json::json!({ "id": "ses_child_a", "parentID": "ses_parent" }),
        ));
        store.apply_event(&event(serde_json::json!({ "id": "ses_unrelated" })));

        let children = store.child_sessions("ses_parent");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].id, "ses_child_a");
        assert_eq!(children[1].id, "ses_child_b");
        assert!(store.child_sessions("ses_unrelated").is_empty());
    }

    #[test]
    fn part_delta_appends_to_text_field() {
        let mut store = MessageStore::default();
        store.upsert_part(text_part("prt_1", "msg_1", ""));
        assert!(store.apply_delta("msg_1", "prt_1", "text", "hel"));
        assert!(store.apply_delta("msg_1", "prt_1", "text", "lo"));
        match &store.parts["msg_1"][0].inner {
            Part::Text { text, .. } => assert_eq!(text, "hello"),
            other => panic!("expected text part, got {other:?}"),
        }
    }

    #[test]
    fn part_delta_before_part_updated_is_dropped() {
        let mut store = MessageStore::default();
        assert!(!store.apply_delta("msg_1", "prt_1", "text", "x"));
        assert!(!store.parts.contains_key("msg_1"));
    }

    #[test]
    fn is_working_reflects_last_message() {
        let mut store = MessageStore::default();
        assert!(!store.is_working("ses_1"), "no messages => idle");
        store.upsert_message(message("msg_1", "ses_1", "user"));
        assert!(store.is_working("ses_1"), "last message is user => working");
        store.upsert_message(message("msg_2", "ses_1", "assistant"));
        assert!(
            store.is_working("ses_1"),
            "assistant without completed time => working"
        );
        let completed: Message = serde_json::from_value(serde_json::json!({
            "id": "msg_2", "sessionID": "ses_1", "role": "assistant",
            "time": { "created": 1, "completed": 2 }
        }))
        .unwrap();
        store.upsert_message(completed);
        assert!(!store.is_working("ses_1"), "assistant completed => idle");
    }

    #[test]
    fn todo_and_diff_events_populate_sidebar_stores() {
        let mut store = MessageStore::default();
        let todo_event: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "todo.updated", "properties": {
                "sessionID": "ses_1",
                "todos": [{ "content": "write tests", "status": "in_progress" }]
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&todo_event));
        assert_eq!(store.todos("ses_1").len(), 1);

        let diff_event: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "session.diff", "properties": {
                "sessionID": "ses_1",
                "diff": [{ "file": "src/main.rs", "additions": 3, "deletions": 1 }]
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&diff_event));
        assert_eq!(store.diffs("ses_1").len(), 1);
        assert_eq!(store.diffs("ses_2").len(), 0);
    }

    #[test]
    fn permission_asked_then_replied_lifecycle() {
        let mut store = MessageStore::default();
        let asked: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "permission.asked", "properties": {
                "id": "per_1", "sessionID": "ses_1", "permission": "edit",
                "patterns": ["src/main.rs"], "metadata": { "filepath": "src/main.rs" }, "always": []
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&asked));
        assert_eq!(store.permissions("ses_1").len(), 1);
        assert_eq!(
            store.permissions("ses_1")[0]
                .get("permission")
                .and_then(Value::as_str),
            Some("edit")
        );

        let replied: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "permission.replied", "properties": {
                "sessionID": "ses_1", "requestID": "per_1"
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&replied));
        assert_eq!(store.permissions("ses_1").len(), 0);
    }

    #[test]
    fn question_asked_then_rejected_lifecycle() {
        let mut store = MessageStore::default();
        let asked: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "question.asked", "properties": {
                "id": "que_1", "sessionID": "ses_1",
                "questions": [{
                    "header": "Mode", "question": "Which mode?",
                    "options": [{ "label": "Fast", "description": "Quick answer" }],
                    "multiple": false, "custom": true
                }]
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&asked));
        assert_eq!(store.questions("ses_1").len(), 1);
        assert_eq!(
            store.questions("ses_1")[0]
                .get("questions")
                .and_then(Value::as_array)
                .and_then(|questions| questions.first())
                .and_then(|question| question.get("header"))
                .and_then(Value::as_str),
            Some("Mode")
        );

        let rejected: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "question.rejected", "properties": {
                "sessionID": "ses_1", "requestID": "que_1"
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&rejected));
        assert_eq!(store.questions("ses_1").len(), 0);
    }

    #[test]
    fn hya_envelope_team_events_fold_into_team_projection() {
        // Team events reach the frontend wrapped in a `hya.envelope` global event
        // whose `properties` is the raw backend envelope (`{ seq, ts_millis, event }`).
        let mut store = MessageStore::default();
        let registered: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "hya.envelope", "properties": {
                "seq": 1, "ts_millis": 1,
                "event": {
                    "type": "agent_registered", "session": "ses_root",
                    "agent_session": "ses_child", "handle": "reviewer-3",
                    "agent_type": "reviewer", "mode": "resident"
                }
            }}
        }))
        .unwrap();
        assert!(
            store.apply_event(&registered),
            "team event mutates the store"
        );
        let team = store.team_for("ses_root").expect("scoped team");
        assert_eq!(
            team.roster
                .get("main/reviewer-3")
                .map(|e| e.session.as_str()),
            Some("ses_child"),
            "roster folded from hya.envelope team event"
        );

        let mail: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "hya.envelope", "properties": {
                "seq": 2, "ts_millis": 2,
                "event": {
                    "type": "mail_sent", "session": "ses_root", "from": "main",
                    "to": { "kind": "handle", "id": "reviewer-3" }, "body": "please review"
                }
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&mail));
        let team = store.team_for("ses_root").expect("scoped team");
        assert_eq!(team.inboxes["main/reviewer-3"].len(), 1);
        assert_eq!(team.inboxes["main/reviewer-3"][0].body, "please review");
    }

    #[test]
    fn hya_envelope_team_rosters_are_scoped_by_root_session() {
        let mut store = MessageStore::default();
        let event = |root: &str, child: &str, status: Option<&str>| -> GlobalEvent {
            let mut inner = serde_json::json!({
                "type": "agent_registered",
                "session": root,
                "agent_session": child,
                "handle": "reviewer-1",
                "agent_type": "reviewer",
                "mode": "resident"
            });
            if let Some(status) = status {
                inner = serde_json::json!({
                    "type": "agent_activity_changed",
                    "session": root,
                    "handle": "reviewer-1",
                    "status": status,
                    "current_task": format!("reviewing {root}")
                });
            }
            serde_json::from_value(serde_json::json!({
                "payload": { "type": "hya.envelope", "properties": { "seq": 1, "event": inner } }
            }))
            .unwrap()
        };

        assert!(store.apply_event(&event("ses_a", "ses_a_child", None)));
        assert!(store.apply_event(&event("ses_b", "ses_b_child", None)));
        assert!(store.apply_event(&event("ses_a", "", Some("busy"))));

        let team_a = store.team_for("ses_a").expect("team a");
        let team_b = store.team_for("ses_b").expect("team b");

        assert_eq!(team_a.roster["main/reviewer-1"].session, "ses_a_child");
        assert_eq!(team_a.roster["main/reviewer-1"].status, "busy");
        assert_eq!(
            team_a.roster["main/reviewer-1"].current_task.as_deref(),
            Some("reviewing ses_a")
        );
        assert_eq!(team_b.roster["main/reviewer-1"].session, "ses_b_child");
        assert_eq!(team_b.roster["main/reviewer-1"].status, "idle");
        assert_eq!(team_b.roster["main/reviewer-1"].current_task, None);
    }

    #[test]
    fn team_root_for_walks_parent_chain_and_defaults_safely() {
        fn session_created(id: &str, parent_id: Option<&str>) -> GlobalEvent {
            let mut info = serde_json::json!({ "id": id });
            if let Some(parent_id) = parent_id {
                info["parentID"] = serde_json::Value::String(parent_id.to_owned());
            }
            serde_json::from_value(serde_json::json!({
                "payload": { "type": "session.created", "properties": { "info": info } }
            }))
            .unwrap()
        }

        let mut store = MessageStore::default();
        assert_eq!(store.team_root_for("missing"), "missing");

        assert!(store.apply_event(&session_created("ses_child", Some("ses_root"))));
        assert!(store.apply_event(&session_created("ses_grandchild", Some("ses_child"))));

        assert_eq!(store.team_root_for("ses_grandchild"), "ses_root");
        assert_eq!(store.team_root_for("ses_child"), "ses_root");
        assert_eq!(store.team_root_for("ses_root"), "ses_root");

        assert!(store.apply_event(&session_created("cycle_a", Some("cycle_b"))));
        assert!(store.apply_event(&session_created("cycle_b", Some("cycle_a"))));
        assert_eq!(store.team_root_for("cycle_a"), "cycle_a");
    }

    #[test]
    fn team_root_for_resolves_unique_roster_owner() {
        let registered = |root: &str, child: &str, handle: &str| -> GlobalEvent {
            serde_json::from_value(serde_json::json!({
                "payload": { "type": "hya.envelope", "properties": {
                    "event": {
                        "type": "agent_registered", "session": root,
                        "agent_session": child, "handle": handle,
                        "agent_type": "reviewer", "mode": "resident"
                    }
                }}
            }))
            .unwrap()
        };
        let mut store = MessageStore::default();
        let child: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "session.created", "properties": {
                "info": { "id": "ses_child" }
            }}
        }))
        .unwrap();

        assert!(store.apply_event(&child));
        assert!(store.apply_event(&registered("ses_root", "ses_child", "reviewer-1")));
        assert_eq!(store.team_root_for("ses_child"), "ses_root");

        assert!(store.apply_event(&registered("ses_other", "ses_child", "reviewer-2")));
        assert_eq!(store.team_root_for("ses_child"), "ses_child");

        let mut own_team = MessageStore::default();
        assert!(own_team.apply_event(&registered("ses_root", "ses_child", "reviewer-1")));
        assert!(own_team.apply_event(&registered("ses_child", "ses_nested", "reviewer-2")));
        assert_eq!(own_team.team_root_for("ses_child"), "ses_child");
    }

    #[test]
    fn session_updated_captures_title() {
        let mut store = MessageStore::default();
        let event: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "session.updated", "properties": {
                "info": { "id": "ses_1", "title": "My Session" }
            }}
        }))
        .unwrap();
        assert!(store.apply_event(&event));
        assert_eq!(
            store.session("ses_1").and_then(|s| s.title.as_deref()),
            Some("My Session")
        );
    }

    #[test]
    fn cap_100_drops_oldest_message_and_its_parts() {
        let mut store = MessageStore::default();
        for index in 0..101 {
            let id = format!("msg_{index:04}");
            store.upsert_message(message(&id, "ses_1", "user"));
            store.upsert_part(text_part(&format!("prt_{index:04}"), &id, "x"));
        }
        let list = &store.messages["ses_1"];
        assert_eq!(list.len(), MESSAGE_CAP);
        assert_eq!(list[0].id, "msg_0001", "oldest message dropped");
        assert!(
            !store.parts.contains_key("msg_0000"),
            "dropped message's parts removed"
        );
    }

    #[test]
    fn replay_live_capture_builds_user_and_assistant_text() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/live_session_turn.jsonl"
        );
        let raw =
            std::fs::read_to_string(path).expect("tests/fixtures/live_session_turn.jsonl missing");
        let mut store = MessageStore::default();
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let event: GlobalEvent = serde_json::from_str(line).expect("parse fixture line");
            if event.is_sync_envelope() || event.is_heartbeat() {
                continue;
            }
            store.apply_event(&event);
        }
        let sessions: Vec<_> = store.messages.keys().cloned().collect();
        assert_eq!(sessions.len(), 1, "one session in the capture");
        let messages = &store.messages[&sessions[0]];
        assert!(
            messages.iter().any(|m| m.role.as_deref() == Some("user")),
            "user message present"
        );

        let assistant = messages
            .iter()
            .find(|m| m.role.as_deref() == Some("assistant"))
            .expect("assistant message present");
        let parts = &store.parts[&assistant.id];
        let assistant_text: String = parts
            .iter()
            .filter_map(|p| match &p.inner {
                Part::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            assistant_text.contains("hello there friend"),
            "assistant streamed reply accumulated, got {assistant_text:?}"
        );
    }
    #[test]
    fn workflow_activity_joins_only_referenced_members_and_deduplicates_counts() {
        let identity = crate::workflow::WorkflowIdentity {
            source: "bundle:demo".to_owned(),
            name: "demo".to_owned(),
            revision: "ab".repeat(32),
        };
        let stage =
            |id: &str,
             level: usize,
             status: WorkflowStageStatus,
             members: Vec<WorkflowMemberProjection>| WorkflowStageProjection {
                plan: crate::workflow::WorkflowStagePlan {
                    id: id.to_owned(),
                    title: Some(format!("Stage {id}")),
                    agent: "build".to_owned(),
                    mode: "once".to_owned(),
                    level,
                },
                status,
                members,
            };
        let reference = |member: &str, iteration: u32| WorkflowMemberProjection {
            member: member.to_owned(),
            role: crate::workflow::WorkflowMemberRole::Worker,
            iteration,
        };
        let workflow = WorkflowProjection {
            selection: Some(identity.clone()),
            availability: None,
            run: Some(WorkflowRunProjection {
                id: "run-1".to_owned(),
                workflow: identity,
                request_hash: "hash".to_owned(),
                owner: "owner-1".to_owned(),
                status: crate::workflow::WorkflowRunStatus::Running,
                stages: vec![
                    stage(
                        "prepare",
                        0,
                        WorkflowStageStatus::Completed,
                        vec![reference("member-1", 0), reference("member-1", 0)],
                    ),
                    stage(
                        "build",
                        1,
                        WorkflowStageStatus::Running,
                        vec![reference("member-1", 0), reference("member-2", 0)],
                    ),
                ],
                error: None,
            }),
        };
        let session: Session = serde_json::from_value(serde_json::json!({
            "id": "ses_workflow",
            "workflow": workflow,
        }))
        .unwrap();
        let mut store = MessageStore::default();
        store.sessions.insert(session.id.clone(), session);
        store.members.insert(
            "ses_workflow".to_owned(),
            vec![
                MemberProjection {
                    member: "member-1".to_owned(),
                    child: Some("ses_child_1".to_owned()),
                    subagent_type: "build".to_owned(),
                    description: "prepare".to_owned(),
                    depth: 0,
                    status: "running".to_owned(),
                    summary: String::new(),
                },
                MemberProjection {
                    member: "member-2".to_owned(),
                    child: Some("ses_child_2".to_owned()),
                    subagent_type: "review".to_owned(),
                    description: "review".to_owned(),
                    depth: 0,
                    status: "done".to_owned(),
                    summary: "ok".to_owned(),
                },
                MemberProjection {
                    member: "unrelated".to_owned(),
                    child: Some("ses_unrelated".to_owned()),
                    subagent_type: "other".to_owned(),
                    description: "must not count".to_owned(),
                    depth: 0,
                    status: "running".to_owned(),
                    summary: String::new(),
                },
            ],
        );

        let activity = store.workflow_activity("ses_workflow").expect("activity");
        assert_eq!(activity.active_agents, 1);
        assert_eq!(activity.total_agents, 2);
        assert_eq!(activity.completed_stages, 1);
        assert_eq!(activity.total_stages, 2);
        assert_eq!(activity.graph_level, Some(1));
        assert_eq!(activity.running_stage_ids, vec!["build"]);
        assert_eq!(
            activity
                .current_stage
                .as_ref()
                .map(|stage| stage.stage.plan.id.as_str()),
            Some("build")
        );
        assert_eq!(activity.stages[0].members.len(), 1);
        assert_eq!(activity.stages[1].members.len(), 2);
        assert!(activity
            .stages
            .iter()
            .flat_map(|stage| stage.members.iter())
            .all(|member| member.member.member != "unrelated"));
    }
    #[test]
    fn session_updated_replaces_workflow_state_without_touching_transcript() {
        let workflow = |name: &str| {
            serde_json::json!({
                "selection": {
                    "source": format!("bundle:{name}"),
                    "name": name,
                    "revision": "ab".repeat(32)
                },
                "run": null
            })
        };
        let session_event = |kind: &str, name: &str| -> GlobalEvent {
            serde_json::from_value(serde_json::json!({
                "payload": { "type": kind, "properties": {
                    "info": { "id": "ses_workflow", "title": "kept", "workflow": workflow(name) }
                }}
            }))
            .unwrap()
        };
        let message_event: GlobalEvent = serde_json::from_value(serde_json::json!({
            "payload": { "type": "message.updated", "properties": {
                "info": { "id": "msg_1", "sessionID": "ses_workflow", "role": "user" }
            }}
        }))
        .unwrap();
        let mut store = MessageStore::default();
        assert!(store.apply_event(&session_event("session.created", "alpha")));
        assert!(store.apply_event(&message_event));
        assert!(store.apply_event(&session_event("session.updated", "beta")));

        let session = store.session("ses_workflow").expect("hydrated session");
        assert_eq!(session.title.as_deref(), Some("kept"));
        assert_eq!(
            session
                .workflow
                .as_ref()
                .and_then(|state| state.selection.as_ref())
                .map(|selection| selection.name.as_str()),
            Some("beta")
        );
        assert_eq!(store.messages["ses_workflow"].len(), 1);
        assert_eq!(store.messages["ses_workflow"][0].id, "msg_1");
    }
}
