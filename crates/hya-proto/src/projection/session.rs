use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::compaction::ContextStatusProjection;
use super::helpers::{find_part, push_part, tool_input, upsert_tool};
use super::{MemberProjection, Projection};
use crate::event::{Event, FileStamp, SessionKind};
use crate::ids::{ConfigGeneration, MessageId, PartId, ProjectId, SessionId, ToolCallId};
use crate::message::{FinishCause, FinishReason, Role, TokenUsage, ToolPartState};
use crate::model::{AgentName, ModelRef, ToolName};
use crate::revert::{FileChangeRecord, FileRestore, RevertProjection};
use crate::usage::{MessageUsage, SessionUsage};
use crate::workflow::WorkflowProjection;

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

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
    /// Initial environment and workspace guidance recorded for full-mode turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_context: Option<String>,
    /// Project the session belongs to (ADR-0024), from `SessionCreated`.
    /// `None` for temporary sessions and sessions created before Projects.
    /// Omitted from serialized projections when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// Project or temporary session (ADR-0024), from `SessionCreated`;
    /// [`SessionKind::Project`] for sessions created before the field.
    #[serde(default)]
    pub kind: SessionKind,
    /// Temporary Agent model choices shared by this root Session tree.
    ///
    /// The map is event-sourced on the root log and omitted from serialized
    /// projections when empty for compatibility with older clients.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_model_overrides: BTreeMap<String, ModelRef>,
    /// Permission mode of this root Session tree (`manual`, `yolo`, or
    /// `<bundle-id>/<mode-id>`), folded from the newest
    /// `SessionPermissionModeSet` on the root log. `None` means the process
    /// default applies. Omitted from serialized projections when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Display title when set.
    pub title: Option<String>,
    /// Opaque session metadata object.
    pub metadata: Option<serde_json::Value>,
    /// Latest terminal handoff document (ADR-0015): the only state an agent
    /// carries between episodes; revival seeds model context from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffProjection>,
    /// Archive stamp (unix epoch milliseconds) while this root session is
    /// archived; `None` when it is not (never archived or
    /// unarchived). See [`SessionProjection::is_archived`].
    pub archived: Option<serde_json::Number>,
    /// Created ephemeral (`SessionEphemeralSet { ephemeral: true }`) and
    /// still unused: the server deletes such a session once no client
    /// watches it. The session's first message (`MessageStarted`), a title,
    /// an archive, or `SessionEphemeralSet { ephemeral: false }` clear it for
    /// good. Omitted from serialized projections when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ephemeral: bool,
    /// Ordered transcript messages.
    pub messages: Vec<MessageProjection>,
    /// Subagents spawned by this session, folded from member lifecycle events.
    /// Empty for sessions that never spawned subagents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberProjection>,
    /// Durable Workflow control state; independent from transcript messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowProjection>,
    /// Latest window-occupancy report for the session, folded from the newest
    /// `ContextStatus` event. `None` until a streaming round has reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_status: Option<ContextStatusProjection>,
    /// Source session when this session was forked (`SessionForked`).
    ///
    /// Copied messages carry the source's `MessageFinished.tokens`; the usage
    /// fold never counts those for a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<SessionId>,
    /// Source-log message the fork was cut before (`SessionForked
    /// .before_message`); `None` for a head fork or a non-fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_before: Option<MessageId>,
    /// Most recent observation for each known file, including deletions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, crate::event::FileStamp>,
    /// Pending revert (`SessionReverted`): the hidden messages live here, not
    /// in `messages`, until `SessionUnreverted` restores them or the next
    /// `MessageStarted` commits the revert. `None` when nothing is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<RevertProjection>,
    /// Billed provider usage of this session, by serving model and purpose.
    ///
    /// Folded from `UsageRecorded` (plus legacy `MessageFinished.tokens` of
    /// messages without records). Never decremented by message deletion,
    /// revert, or compaction.
    #[serde(default, skip_serializing_if = "SessionUsage::is_empty")]
    pub usage: SessionUsage,
    /// Latest todo list, folded from `TodosUpdated` or a historical todo result.
    /// `None` when the session never recorded a list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<crate::TodoItem>>,
    /// Highest issued numeric todo id. Stored in the durable snapshot only.
    #[serde(skip)]
    pub todo_last_id: u64,
    /// Count of this session's `FilesObserved` and `FilesChanged` events: a
    /// revision of the workspace record that changes whenever it does.
    /// Stored in the durable snapshot only.
    #[serde(skip)]
    pub file_revision: u64,
}

/// One message row in a folded session transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageProjection {
    /// Message id.
    pub id: MessageId,
    /// Speaker role.
    pub role: Role,
    /// Agent the assistant turn ran as, from `MessageStarted`. `None` for
    /// user/system/shell messages and logs written before attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentName>,
    /// Model the assistant turn requested, from `MessageStarted`. The model
    /// that served it is [`MessageProjection::served_model`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Envelope time (Unix ms) of the message's `MessageStarted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_created: Option<i64>,
    /// Envelope time (Unix ms) of the newest event that changed the message
    /// (parts, live deltas included, usage, error, finish).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_updated: Option<i64>,
    /// Runtime snapshot generation from `TurnBindingRecorded` (assistant turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_generation: Option<ConfigGeneration>,
    /// Finish reason when the message is closed.
    pub finish: Option<FinishReason>,
    /// Harness cause recorded on `MessageFinished` (cancel, shutdown, crash
    /// recovery, provider failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<FinishCause>,
    /// Usage recorded on `MessageFinished` (legacy per-message sum).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    /// Attributed usage folded from this message's `UsageRecorded` rounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MessageUsage>,
    /// Prompt file attachments from `UserPromptContextRecorded`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<serde_json::Value>,
    /// Prompt agent mentions from `UserPromptContextRecorded`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<serde_json::Value>,
    /// Ordered content parts (text / reasoning / tool only — no media).
    pub parts: Vec<PartProjection>,
    /// Part offset at the newest `StepStarted`; absent before the first step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_step_start: Option<usize>,
    /// Leading parts folded into a compaction summary by a step cut
    /// (`ContextCompacted.step_cut`). They stay here for readers of the
    /// transcript; requests send only the parts after them.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub folded_parts: usize,
    /// Files this message's tool calls changed, with their prior state
    /// (`FilesChanged`), in call order. What a revert restores from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_changes: Vec<FileChangeRecord>,
    /// Why the turn driving this message failed, folded from an `Error`
    /// event naming it in `failed_message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<MessageError>,
}

impl MessageProjection {
    /// Model that produced this message: the model that served its latest
    /// recorded round (`UsageRecorded`, after fallback or routing) when one
    /// exists, otherwise the model its turn requested.
    #[must_use]
    pub fn served_model(&self) -> Option<&ModelRef> {
        self.usage
            .as_ref()
            .map(|usage| &usage.model)
            .or(self.model.as_ref())
    }
}

/// Recorded failure of the turn that drove an assistant message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageError {
    /// Machine-readable code (for example `provider_error`).
    pub code: String,
    /// Human-readable error text as the engine recorded it.
    pub message: String,
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
    /// Accumulated reasoning plus optional provider blob and requested effort.
    Reasoning {
        /// Part id.
        id: PartId,
        /// Full reasoning text so far.
        text: String,
        /// Requested reasoning effort, when the originating request supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
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
        /// Text requests send instead of the completed output, set by
        /// [`Event::ContextEvicted`]. `state` keeps the full output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        eviction_notice: Option<String>,
        /// Opaque provider-native metadata recorded when the call was requested.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
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

impl SessionProjection {
    /// Whether this (root) session is archived.
    #[must_use]
    pub fn is_archived(&self) -> bool {
        self.archived.is_some()
    }

    /// When this session was archived (unix epoch milliseconds); `None`
    /// when it is not archived.
    #[must_use]
    pub fn archived_at_millis(&self) -> Option<i64> {
        let stamp = self.archived.as_ref()?;
        stamp.as_i64().or_else(|| {
            stamp
                .as_f64()
                .filter(|value| value.is_finite())
                .map(|value| value as i64)
        })
    }
}

/// Why a session's archive state cannot change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionArchiveError {
    /// The projection has no `SessionCreated`: the session does not exist.
    NotFound,
    /// Only root sessions are archived; subagent children follow their root.
    NotRoot,
}

impl std::fmt::Display for SessionArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("session not found"),
            Self::NotRoot => f.write_str("only root sessions can be archived"),
        }
    }
}

impl std::error::Error for SessionArchiveError {}

/// The event that moves `projection`'s session to `archived`, or `None` when
/// it already is there (archiving is idempotent and keeps the first stamp).
///
/// `now_millis` stamps a new archive. Unarchiving a subagent child is a
/// no-op (children are never archived); archiving one is
/// [`SessionArchiveError::NotRoot`].
///
/// # Errors
/// [`SessionArchiveError::NotFound`] for an empty projection, and
/// [`SessionArchiveError::NotRoot`] when archiving a child session.
pub fn session_archive_event(
    projection: &Projection,
    archived: bool,
    now_millis: i64,
) -> Result<Option<Event>, SessionArchiveError> {
    let session = projection.session.id.ok_or(SessionArchiveError::NotFound)?;
    if archived == projection.session.is_archived() {
        return Ok(None);
    }
    if !archived {
        return Ok(Some(Event::SessionUnarchived { session }));
    }
    if projection.session.parent.is_some() {
        return Err(SessionArchiveError::NotRoot);
    }
    Ok(Some(Event::SessionArchived {
        session,
        archived: serde_json::Number::from(now_millis.max(1)),
    }))
}

/// The transcript message an event folds onto, if any (drives
/// `MessageProjection::time_created` / `time_updated`). Step boundaries do not
/// change content timestamps; switch/compaction anchors only point at a message.
fn changed_message(event: &Event) -> Option<MessageId> {
    match event {
        Event::MessageStarted { message, .. }
        | Event::TurnBindingRecorded { message, .. }
        | Event::UserPromptContextRecorded { message, .. }
        | Event::MessageFinished { message, .. }
        | Event::PartDeleted { message, .. }
        | Event::TextStart { message, .. }
        | Event::TextDelta { message, .. }
        | Event::TextReplace { message, .. }
        | Event::TextEnd { message, .. }
        | Event::ReasoningStart { message, .. }
        | Event::ReasoningDelta { message, .. }
        | Event::ReasoningEnd { message, .. }
        | Event::ReasoningReplace { message, .. }
        | Event::ToolInputStart { message, .. }
        | Event::ToolCallRequested { message, .. }
        | Event::ToolResult { message, .. }
        | Event::ToolError { message, .. }
        | Event::ToolPartUpdated { message, .. }
        | Event::FilesChanged { message, .. } => Some(*message),
        Event::UsageRecorded { message, .. } => *message,
        Event::Error { failed_message, .. } => *failed_message,
        _ => None,
    }
}

impl Projection {
    /// Fold the envelope time onto the message the event changed: its
    /// `MessageStarted` time is the creation time, and every change
    /// (including live deltas) moves the update time forward.
    pub(super) fn stamp_message_time(&mut self, event: &Event, ts_millis: i64) {
        let Some(id) = changed_message(event) else {
            return;
        };
        let Some(message) = self.message_mut(id) else {
            return;
        };
        if matches!(event, Event::MessageStarted { .. }) && message.time_created.is_none() {
            message.time_created = Some(ts_millis);
        }
        message.time_updated = Some(message.time_updated.map_or(ts_millis, |t| t.max(ts_millis)));
    }

    pub(super) fn replace_todos(&mut self, todos: Vec<crate::TodoItem>) {
        for id in todos.iter().filter_map(|item| item.id.parse::<u64>().ok()) {
            self.session.todo_last_id = self.session.todo_last_id.max(id);
        }
        self.session.todos = Some(todos);
    }

    /// Streaming events almost always target the newest message, so search
    /// from the back; message ids are unique in the list, so the result is the
    /// same as a forward search.
    pub(super) fn message_mut(&mut self, id: MessageId) -> Option<&mut MessageProjection> {
        self.session.messages.iter_mut().rev().find(|m| m.id == id)
    }

    /// Hide `message` and every later message behind a pending revert. A
    /// revert while one is pending extends it: the older hidden messages
    /// follow the newly hidden ones, and a file keeps the `saved` state of the
    /// first revert (the state an unrevert must return to).
    pub(super) fn apply_revert(&mut self, message: MessageId, files: &[FileRestore]) {
        let Some(index) = self
            .session
            .messages
            .iter()
            .position(|item| item.id == message)
        else {
            return;
        };
        let mut hidden = self.session.messages.split_off(index);
        let mut merged: Vec<FileRestore> = files.to_vec();
        if let Some(previous) = self.session.revert.take() {
            hidden.extend(previous.hidden);
            for old in previous.files {
                match merged.iter_mut().find(|file| file.path == old.path) {
                    Some(file) => file.saved = old.saved,
                    None => merged.push(old),
                }
            }
        }
        self.session.revert = Some(RevertProjection {
            message,
            hidden,
            files: merged,
        });
    }

    pub(super) fn apply_message_started(
        &mut self,
        message: &MessageId,
        role: &Role,
        agent: &Option<AgentName>,
        model: &Option<ModelRef>,
    ) {
        // Any message makes the session used: never ephemeral again.
        self.session.ephemeral = false;
        if self.message_mut(*message).is_none() {
            // A new message commits a pending revert: the hidden
            // messages are gone for good and cannot be restored.
            self.session.revert = None;
            self.session.messages.push(MessageProjection {
                id: *message,
                role: *role,
                agent: agent.clone(),
                model: model.clone(),
                time_created: None,
                time_updated: None,
                config_generation: None,
                finish: None,
                cause: None,
                tokens: None,
                usage: None,
                files: Vec::new(),
                agents: Vec::new(),
                parts: Vec::new(),
                last_step_start: None,
                folded_parts: 0,
                file_changes: Vec::new(),
                error: None,
            });
        }
    }

    pub(super) fn apply_step_started(&mut self, message: &MessageId) {
        if let Some(message) = self.message_mut(*message) {
            message.last_step_start = Some(message.parts.len());
        }
    }

    pub(super) fn apply_turn_binding_recorded(
        &mut self,
        message: &MessageId,
        generation: &ConfigGeneration,
    ) {
        if let Some(message) = self.message_mut(*message) {
            message.config_generation = Some(*generation);
        }
    }

    pub(super) fn apply_user_prompt_context_recorded(
        &mut self,
        message: &MessageId,
        files: &[serde_json::Value],
        agents: &[serde_json::Value],
    ) {
        if let Some(message) = self.message_mut(*message) {
            message.files = files.to_vec();
            message.agents = agents.to_vec();
        }
    }

    pub(super) fn apply_message_finished(
        &mut self,
        message: &MessageId,
        finish: &FinishReason,
        tokens: &Option<TokenUsage>,
        cause: &Option<FinishCause>,
    ) {
        let forked = self.session.forked_from.is_some();
        let mut legacy = None;
        if let Some(m) = self.message_mut(*message) {
            // Legacy fallback: a message with no per-round records
            // (a log written before `UsageRecorded`) contributes its
            // sum once, unattributed. A fork's copies never count.
            if let Some(sum) = tokens
                && !sum.is_zero()
                && m.usage.is_none()
                && m.tokens.is_none()
                && !forked
            {
                legacy = Some(*sum);
            }
            m.finish = Some(*finish);
            m.cause = *cause;
            m.tokens = *tokens;
        }
        if let Some(sum) = legacy {
            self.session.usage.record_legacy(&sum);
        }
    }

    pub(super) fn apply_files_changed(
        &mut self,
        message: &MessageId,
        call: &Option<ToolCallId>,
        files: &[crate::revert::FileChange],
    ) {
        self.session.file_revision += 1;
        if let Some(message) = self.message_mut(*message) {
            message
                .file_changes
                .extend(files.iter().map(|file| FileChangeRecord {
                    call: *call,
                    path: file.path.clone(),
                    before: file.before.clone(),
                    observed: file.observed,
                }));
        }
    }

    pub(super) fn apply_files_observed(&mut self, files: &[FileStamp]) {
        self.session.file_revision += 1;
        self.session
            .files
            .extend(files.iter().map(|file| (file.path.clone(), file.clone())));
    }

    pub(super) fn apply_session_unreverted(&mut self) {
        if let Some(revert) = self.session.revert.take() {
            self.session.messages.extend(revert.hidden);
        }
    }

    pub(super) fn apply_part_deleted(&mut self, message: &MessageId, part: &PartId) {
        if let Some(message) = self.message_mut(*message)
            && let Some(at) = message.parts.iter().position(|item| item.id() == *part)
        {
            message.parts.remove(at);
            if let Some(start) = &mut message.last_step_start
                && at < *start
            {
                *start -= 1;
            }
            if at < message.folded_parts {
                message.folded_parts -= 1;
            }
        }
    }

    pub(super) fn apply_text_start(&mut self, message: &MessageId, part: &PartId) {
        push_part(
            self,
            *message,
            PartProjection::Text {
                id: *part,
                text: String::new(),
            },
        );
    }

    pub(super) fn apply_text_delta(&mut self, message: &MessageId, part: &PartId, delta: &str) {
        if let Some(PartProjection::Text { text, .. }) = find_part(self, *message, *part) {
            text.push_str(delta);
        }
    }

    pub(super) fn apply_text_replace(
        &mut self,
        message: &MessageId,
        part: &PartId,
        replacement: &str,
    ) {
        if let Some(PartProjection::Text { text, .. }) = find_part(self, *message, *part) {
            *text = replacement.to_owned();
        }
    }

    pub(super) fn apply_reasoning_start(
        &mut self,
        message: &MessageId,
        part: &PartId,
        reason: &Option<String>,
    ) {
        push_part(
            self,
            *message,
            PartProjection::Reasoning {
                id: *part,
                text: String::new(),
                reason: reason.clone(),
                provider_data: None,
            },
        );
    }

    pub(super) fn apply_reasoning_delta(
        &mut self,
        message: &MessageId,
        part: &PartId,
        delta: &str,
    ) {
        if let Some(PartProjection::Reasoning { text, .. }) = find_part(self, *message, *part) {
            text.push_str(delta);
        }
    }

    pub(super) fn apply_reasoning_replace(
        &mut self,
        message: &MessageId,
        part: &PartId,
        replacement: &str,
    ) {
        if let Some(PartProjection::Reasoning { text, .. }) = find_part(self, *message, *part) {
            *text = replacement.to_owned();
        }
    }

    pub(super) fn apply_reasoning_end(
        &mut self,
        message: &MessageId,
        part: &PartId,
        provider_data: &Option<serde_json::Value>,
    ) {
        if let Some(PartProjection::Reasoning {
            provider_data: stored,
            ..
        }) = find_part(self, *message, *part)
        {
            *stored = provider_data.clone();
        }
    }

    pub(super) fn apply_tool_input_start(
        &mut self,
        message: &MessageId,
        part: &PartId,
        call: &ToolCallId,
        name: &ToolName,
    ) {
        push_part(
            self,
            *message,
            PartProjection::Tool {
                id: *part,
                call: *call,
                name: name.clone(),
                state: ToolPartState::Pending {
                    input: serde_json::Value::Null,
                },
                eviction_notice: None,
                provider_data: None,
            },
        );
    }

    pub(super) fn apply_tool_call_requested(
        &mut self,
        message: &MessageId,
        part: &PartId,
        call: &ToolCallId,
        name: &ToolName,
        input: &serde_json::Value,
        provider_data: &Option<serde_json::Value>,
    ) {
        upsert_tool(
            self,
            *message,
            *part,
            *call,
            name.clone(),
            ToolPartState::Running {
                input: input.clone(),
            },
            provider_data.clone(),
        );
    }

    pub(super) fn apply_tool_result(
        &mut self,
        message: &MessageId,
        part: &PartId,
        output: &serde_json::Value,
        time_ms: &u64,
    ) {
        if let Some(PartProjection::Tool { name, state, .. }) = find_part(self, *message, *part) {
            let input = tool_input(state);
            let mut added = 0;
            let legacy = crate::todo::legacy_result(name.as_str(), output);
            if name.as_str() == crate::context::TODO_TOOL && output.is_string() {
                // A successful batch consumes an id for every add, even when
                // another op in the same batch removes the added row.
                added = input["ops"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|op| op["op"] == "add")
                    .count() as u64;
            }
            *state = ToolPartState::Completed {
                input,
                output: output.clone(),
                time_ms: *time_ms,
            };
            self.session.todo_last_id += added;
            if let Some(items) = legacy {
                self.replace_todos(items);
            }
        }
    }

    pub(super) fn apply_tool_error(
        &mut self,
        message: &MessageId,
        part: &PartId,
        message_text: &str,
        value: &Option<serde_json::Value>,
    ) {
        if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
            let input = tool_input(state);
            *state = ToolPartState::Error {
                input,
                message: message_text.to_owned(),
                value: value.clone(),
            };
        }
    }

    pub(super) fn apply_tool_part_updated(
        &mut self,
        message: &MessageId,
        part: &PartId,
        next: &ToolPartState,
    ) {
        if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
            *state = next.clone();
        }
    }

    pub(super) fn apply_handoff_committed(&mut self, generation: &u32, doc: &str, degraded: &bool) {
        self.session.handoff = Some(HandoffProjection {
            generation: *generation,
            doc: doc.to_owned(),
            degraded: *degraded,
        });
    }

    pub(super) fn apply_error(&mut self, message: &MessageId, code: &str, text: &str) {
        if let Some(m) = self.message_mut(*message) {
            m.error = Some(MessageError {
                code: code.to_owned(),
                message: text.to_owned(),
            });
        }
    }
}
