//! The canonical streaming `Event` (design.md §3) + its ordered `Envelope`.
//!
//! Phase 1 defines the core agent-loop events (session/message/step/text/
//! reasoning/tool/error). Team, goal, and loop event variants are additive and
//! land with their phases.
//!
//! Wire form: `#[serde(tag = "type", rename_all = "snake_case")]`. Consumers
//! treat envelopes as the unit of store replay and SSE; see
//! `docs/architecture/event-model.md`.

use serde::{Deserialize, Serialize};

use crate::ids::{ConfigGeneration, EventSeq, MessageId, PartId, ProjectId, SessionId, ToolCallId};
use crate::message::{FinishCause, FinishReason, Role, TokenUsage, ToolPartState, UsagePurpose};
use crate::model::{AgentName, ModelRef, ToolName};

/// Kind of a session (ADR-0024): part of a Project, or a temporary session
/// with its own scratch workdir that belongs to no Project.
///
/// Wire form is snake_case (`project` | `temporary`); absent decodes as
/// [`SessionKind::Project`], the kind every pre-ADR-0024 session has.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// A session working inside a Project's roots (or a legacy session with
    /// no Project recorded).
    #[default]
    Project,
    /// A temporary session: no Project, workdir is a fresh scratch directory.
    Temporary,
}

impl SessionKind {
    /// Wire/storage label: `project` or `temporary`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Temporary => "temporary",
        }
    }
}

/// Reference to the view used to build a model round's request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ViewRef {
    /// Last durable event in the view's session log prefix (inclusive).
    pub cut: EventSeq,
    /// Lowercase hex SHA-256 of the canonical system, tools, and messages.
    pub request_sha256: String,
}

/// Canonical runtime event stream: one tagged variant per discrete state change.
///
/// Persist durable variants with a real `EventSeq`; high-frequency text may be
/// published live at `seq == 0` and re-emitted durably after the stream ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // -------- session lifecycle --------
    /// Session is created; fold sets id/parent/agent/model/workdir/project/kind.
    SessionCreated {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent session when this is a child (lineage toward team root).
        parent: Option<SessionId>,
        /// Initial agent binding.
        agent: AgentName,
        /// Initial model binding.
        model: ModelRef,
        /// Absolute workdir for tools.
        workdir: String,
        /// Project the session belongs to (ADR-0024). `None` for temporary
        /// sessions and for logs written before Projects existed. Subagent
        /// sessions carry their parent's Project.
        #[serde(default)]
        project: Option<ProjectId>,
        /// Project or temporary session (ADR-0024). Logs written before the
        /// field existed decode as [`SessionKind::Project`].
        #[serde(default)]
        kind: SessionKind,
    },

    /// Freeze the initial environment and workspace guidance for full-mode turns.
    InitialContextRecorded {
        /// Session this event belongs to.
        session: SessionId,
        /// Rendered initial context used in the system prompt.
        text: String,
    },

    /// Discard the recorded initial context (`/reload-context`): the next
    /// full-mode turn renders it again from the files as they are then and
    /// records a new [`Event::InitialContextRecorded`].
    InitialContextInvalidated {
        /// Session this event belongs to.
        session: SessionId,
    },

    /// Set or clear one temporary Agent model for the owning root Session tree.
    /// A `None` model removes the override; unrelated Agent entries remain.
    SessionAgentModelOverrideSet {
        /// Root Session whose descendant tree receives this override.
        session: SessionId,
        /// Stable Agent id whose temporary model changed.
        agent: AgentName,
        /// Temporary model, or `None` to clear it.
        model: Option<ModelRef>,
    },
    /// Set the permission mode of the owning root Session tree.
    ///
    /// Appended to the lineage root; descendants (subagent sessions) inherit
    /// it. `mode` is `manual`, `yolo`, or `<bundle-id>/<mode-id>` for a mode
    /// declared by an installed bundle's `permission_modes:`. The last write
    /// wins.
    SessionPermissionModeSet {
        /// Root Session whose descendant tree uses this mode.
        session: SessionId,
        /// Mode identifier (`manual`, `yolo`, or `<bundle-id>/<mode-id>`).
        mode: String,
    },
    /// Session title set or updated.
    SessionTitled {
        /// Session this event belongs to.
        session: SessionId,
        /// Display title.
        title: String,
    },
    /// Arbitrary session metadata replaced wholesale.
    SessionMetadataSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Full metadata object (replaces prior metadata).
        metadata: serde_json::Value,
    },
    /// A root session was archived (hidden from default session lists).
    ///
    /// Folds `SessionProjection.archived` to the stamp. Archiving never
    /// cancels a running turn. `SessionUnarchived` clears the archive.
    SessionArchived {
        /// Session this event belongs to.
        session: SessionId,
        /// Unix epoch milliseconds when the session was archived.
        archived: serde_json::Number,
    },
    /// A root session left the archive: explicitly, or implicitly because a
    /// new prompt or shell turn was admitted on it. Folds
    /// `SessionProjection.archived` to `None`.
    SessionUnarchived {
        /// Session this event belongs to.
        session: SessionId,
    },
    /// A root session was marked ephemeral (`true`: created for a client
    /// before the user asked for one, e.g. the TUI's session on connect) or
    /// kept (`false`: a fork was taken from it). The server deletes an
    /// ephemeral session once it is still unused and no client watches it.
    /// Folds `SessionProjection.ephemeral`; the session's first message, a
    /// title, or an archive also clear it for good.
    SessionEphemeralSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Whether the session is (still) ephemeral.
        ephemeral: bool,
    },
    /// Active agent switched for the session.
    AgentSwitched {
        /// Session this event belongs to.
        session: SessionId,
        /// New agent name.
        agent: AgentName,
    },
    /// Active model switched for the session.
    ModelSwitched {
        /// Session this event belongs to.
        session: SessionId,
        /// New model reference.
        model: ModelRef,
    },
    /// Slash command produced a user message; reducer no-op (compat `command.executed`).
    CommandExecuted {
        /// Session this event belongs to.
        session: SessionId,
        /// Command name without `/`.
        command: String,
        /// Argument string after the command.
        arguments: String,
        /// User message id that was admitted.
        message: MessageId,
    },

    // -------- message lifecycle --------
    /// Opens a message row in the projection (`role` + new `message` id).
    MessageStarted {
        /// Session this event belongs to.
        session: SessionId,
        /// New message id.
        message: MessageId,
        /// Speaker role for the message.
        role: Role,
        /// Agent the assistant turn ran as. Absent on user/system/shell
        /// messages and on logs written before per-message attribution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<AgentName>,
        /// Model the assistant turn requested when it started. The model that
        /// actually served each round (after `chat.params`, fallback, or
        /// routing) is recorded separately by `UsageRecorded`. Absent where
        /// `agent` is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<ModelRef>,
    },
    /// Records which immutable runtime snapshot (`ConfigGeneration`) ran this
    /// assistant turn, with digests of the system prompt and tool list its
    /// requests send. Emitted again mid-turn when either digest changes.
    TurnBindingRecorded {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message the binding applies to.
        message: MessageId,
        /// Lightweight generation identity (registry contents stay outside the log).
        generation: ConfigGeneration,
        /// Lowercase hex SHA-256 of the system prompt text the turn's requests
        /// send. Empty for turns that send no model request (shell, fork) and
        /// in logs written before this field.
        #[serde(default)]
        system_sha256: String,
        /// Lowercase hex SHA-256 of the JSON-serialized tool schema list the
        /// turn's requests offer. Empty where `system_sha256` is.
        #[serde(default)]
        tools_sha256: String,
    },
    /// `@file` / `@agent` prompt context; engine emits nothing when both vectors are empty.
    UserPromptContextRecorded {
        /// Session this event belongs to.
        session: SessionId,
        /// User message the context attaches to.
        message: MessageId,
        /// File attachment metadata for the provider request builder.
        files: Vec<serde_json::Value>,
        /// Agent mention metadata for the provider request builder.
        agents: Vec<serde_json::Value>,
    },
    /// Closes a message; clients that saw start must eventually see finish.
    MessageFinished {
        /// Session this event belongs to.
        session: SessionId,
        /// Message being finished.
        message: MessageId,
        /// Role of the finished message.
        role: Role,
        /// Terminal finish reason.
        finish: FinishReason,
        /// Aggregated token usage when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<TokenUsage>,
        /// Why the harness ended the message (cancel, shutdown, crash
        /// recovery, provider failure). Absent on model-ended messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<FinishCause>,
    },
    /// Removes one part from a message in the projected view.
    PartDeleted {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Part to drop.
        part: PartId,
    },

    // -------- assistant streaming --------
    /// Provider round started; records the message's last-step part boundary.
    StepStarted {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message owning the round.
        message: MessageId,
        /// Zero-based round index within the turn.
        step: u32,
        /// Request view, shared by every retry of the round. Absent in old logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewRef>,
    },
    /// Provider round finished; reducer no-op. `finish` defaults to `stop` on old logs.
    StepFinished {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message owning the round.
        message: MessageId,
        /// Zero-based round index within the turn.
        step: u32,
        /// Provider finish for this round (default `stop` when absent).
        #[serde(default = "default_step_finish_reason")]
        finish: FinishReason,
        /// Bounded single-line error summary when a started step failed (omitted otherwise).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Begin a text part (empty until deltas/replace).
    TextStart {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// New text part id.
        part: PartId,
    },
    /// Append streaming text; field is `delta`, not `text`.
    TextDelta {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target text part.
        part: PartId,
        /// Chunk to append.
        delta: String,
    },
    /// Wholesale text overwrite (durable final content and `text_complete` rewrites).
    TextReplace {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target text part.
        part: PartId,
        /// Full replacement text.
        text: String,
    },
    /// Text stream end marker; reducer no-op (text already accumulated).
    TextEnd {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Closed text part.
        part: PartId,
    },
    /// Begin a reasoning part.
    ReasoningStart {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// New reasoning part id.
        part: PartId,
        /// Requested reasoning effort, when the provider request supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Append reasoning text chunk.
    ReasoningDelta {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target reasoning part.
        part: PartId,
        /// Chunk to append.
        delta: String,
    },
    /// End reasoning and attach opaque `provider_data` for round-trip.
    ReasoningEnd {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Closed reasoning part.
        part: PartId,
        /// Opaque provider state (for example encrypted thinking); round-trip verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    /// Wholesale reasoning text overwrite.
    ReasoningReplace {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target reasoning part.
        part: PartId,
        /// Full replacement reasoning text.
        text: String,
    },

    // -------- tool lifecycle --------
    /// Tool part opened in `Pending` (null input until arguments arrive).
    ToolInputStart {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// New tool part id.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
    },
    /// Live-only raw argument JSON fragment (`seq == 0`), never persisted.
    /// `ToolCallRequested` durably records the complete parsed input.
    ToolInputDelta {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
        /// Argument JSON fragment.
        delta: String,
    },
    /// Model requested a tool call → `Running`; turn loop collects these for dispatch.
    ToolCallRequested {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
        /// Parsed tool arguments.
        input: serde_json::Value,
        /// Opaque provider-native metadata attached to this call (e.g. upstream
        /// `call_id` or provider signature fields).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    /// Tool succeeded → `Completed`.
    ToolResult {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Tool output JSON (may be capped).
        output: serde_json::Value,
        /// Execution duration in milliseconds.
        time_ms: u64,
    },
    /// Tool failed/denied/blocked → `Error`.
    ToolError {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Human/model-facing error string.
        message_text: String,
        /// Optional structured error (for example `{ "error": { "type", "message" } }` or `STALE_ACTOR_CLAIM`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
    /// Direct tool-part state overwrite (fork/copy, out-of-band progress).
    ToolPartUpdated {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Full replacement tool state.
        state: ToolPartState,
    },

    // -------- context rewrite & fork --------
    /// A compaction or kernel rewrite folded part of this session's transcript.
    ///
    /// Appended to the log of the session that compacted; there is no parent
    /// mirror, because `event_log.seq` already orders every agent's events on one
    /// global timeline. The reducer moves the summary `message` to the fold
    /// boundary (right after `to_message`, or before `step_cut.kept_head`) so
    /// request reconstruction starts at it; nothing else changes.
    ///
    /// The folded input is a **pointer**, not a copy. Compaction never deletes,
    /// so `from_message..=to_message` plus the log reconstructs exactly what the
    /// summarizer saw. Range semantics differ by strategy: `Native` folds the
    /// whole input window, `LocalSummarizer` folds the prefix before the
    /// retained recent messages.
    ContextCompacted {
        /// Session whose context was compacted.
        session: SessionId,
        /// System message carrying the compaction marker: the output.
        message: MessageId,
        /// Which compaction path produced it.
        strategy: CompactionStrategy,
        /// First message folded into the summary.
        from_message: MessageId,
        /// Last message folded into the summary.
        to_message: MessageId,
        /// Number of messages folded.
        folded_count: u32,
        /// Estimated input tokens that tripped the threshold.
        input_tokens_est: u64,
        /// Threshold in force when it tripped.
        threshold: u64,
        /// Set when the fold also cut inside `to_message`, the turn's
        /// in-flight assistant message, at a step boundary. Absent in logs
        /// written before step folds existed, which fold whole messages only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_cut: Option<StepCut>,
    },

    /// A session was forked from another session.
    ///
    /// Deliberately **not** `SessionCreated.parent`: `parent` means subagent
    /// lineage and drives depth accounting, governor budgets, and the team root,
    /// so reusing it would make a fork masquerade as a subagent child. The run
    /// tree derives from spawn edges only; a fork is a separate edge type.
    ///
    /// Appended to the forked session's own log. Copied messages get fresh ids,
    /// so the correspondence to the source is positional via `before_message`.
    SessionForked {
        /// The new forked session.
        session: SessionId,
        /// Session it was forked from.
        source: SessionId,
        /// Cut point: the source message (id in the source log) before which
        /// the copy stopped; messages strictly before it were copied. `None`
        /// copied every visible message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_message: Option<MessageId>,
    },

    // -------- token accounting --------
    /// Billed usage of one provider call, attributed to the model that served it.
    ///
    /// Emitted once per provider call that reported usage: every streaming
    /// round of an assistant turn (including rounds of a message that later
    /// finished `cancelled` or `error`), plus the title and summarizer side
    /// calls made on the session's behalf. Side calls carry no `message`/`step`
    /// and never create transcript messages. `tokens` follows the normalized
    /// [`TokenUsage`] invariant for this one call (not a message sum).
    ///
    /// Folds into `SessionProjection.usage` only; `MessageFinished.tokens`
    /// keeps its legacy per-message sum and is not counted again for messages
    /// that have records.
    UsageRecorded {
        /// Session billed for the call.
        session: SessionId,
        /// Assistant message of a `turn` round; `None` for side calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        /// Zero-based round index within the message; `None` for side calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u32>,
        /// Model that served the call (after chat.params, fallback, or route).
        model: ModelRef,
        /// Why the call was made.
        purpose: UsagePurpose,
        /// Provider-reported usage of this call.
        tokens: TokenUsage,
    },

    // -------- todos --------
    /// The session's todo list changed: the full list after a todo tool call.
    ///
    /// Appended by the kernel in the same batch as the `ToolResult` of a
    /// `todo` call that changes the list. Folds into `SessionProjection.todos`.
    TodosUpdated {
        /// Session whose todo list changed.
        session: SessionId,
        /// Full replacement list, in order.
        todos: Vec<crate::TodoItem>,
    },

    // -------- file snapshots and revert --------
    /// Files one tool call changed, each with its content before the change.
    ///
    /// Recorded after the call has changed at least one file (write, edit,
    /// apply_patch, and bash inside a git work tree). Contents live in the
    /// store's per-session blob table; the event carries only paths and blob
    /// keys. Folds onto `MessageProjection.file_changes` of `message` and is
    /// what a revert restores from.
    FilesChanged {
        /// Session whose tool changed the files.
        session: SessionId,
        /// Assistant message of the tool call.
        message: MessageId,
        /// Tool call that changed the files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<ToolCallId>,
        /// Changed files, each with its state before the call.
        files: Vec<crate::revert::FileChange>,
    },

    /// Metadata of files the session has accessed, or changes since the last observation.
    FilesObserved {
        /// Session whose known files are being updated.
        session: SessionId,
        /// True for a change detected before a round, false for a tool observation.
        external: bool,
        /// One stamp per observed path.
        files: Vec<FileStamp>,
    },

    /// The transcript was reverted to just before user message `message`.
    ///
    /// Folds: `message` and every later message move from
    /// `SessionProjection.messages` to `SessionProjection.revert.hidden`.
    /// Recorded after the engine restored the files the hidden turns changed
    /// (`files`). A revert while one is pending extends it further back. The
    /// next `MessageStarted` commits the pending revert (the hidden messages
    /// are dropped for good).
    SessionReverted {
        /// Reverted session.
        session: SessionId,
        /// First hidden message: the reverted user message.
        message: MessageId,
        /// Files restored to their state before `message`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<crate::revert::FileRestore>,
    },

    /// The pending revert was undone: hidden messages return to the
    /// transcript and the files it restored were written back (`files`).
    /// A no-op fold when no revert is pending.
    SessionUnreverted {
        /// Session whose revert was undone.
        session: SessionId,
        /// Files written back to their state before the revert.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<crate::revert::FileRestore>,
    },

    // -------- errors --------
    /// Runtime error frame; `session` optional for global errors.
    ///
    /// With `failed_message` set it records why a turn failed and folds onto
    /// that message (`MessageProjection.error`); otherwise the reducer
    /// ignores it.
    Error {
        /// Session scope when the error is session-local; `None` for global errors.
        session: Option<SessionId>,
        /// Machine-readable error code for clients.
        code: String,
        /// Human-readable error text.
        message: String,
        /// Assistant message the error ended (a failed turn), appended just
        /// before that message's `MessageFinished { finish: error }`. Absent
        /// for errors not tied to a message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed_message: Option<MessageId>,
    },

    // -------- bundle extension events --------
    /// Bundle-defined extension event folded by registered
    /// [`crate::ProjectionReducer`]s and preserved verbatim in the log even
    /// when the owning bundle is not loaded.
    Extension {
        /// Session this event belongs to.
        session: SessionId,
        /// Owning bundle id (for example `hya/workflow` or `hya/multi-agent`).
        bundle: String,
        /// Bundle-defined event kind discriminator.
        kind: String,
        /// Opaque JSON payload owned by the bundle.
        payload: serde_json::Value,
    },
}
fn default_step_finish_reason() -> FinishReason {
    FinishReason::Stop
}

/// Where a [`Event::ContextCompacted`] fold cut inside the in-flight
/// assistant message (`to_message`).
///
/// One assistant message holds every step of a turn, so a long single turn has
/// no whole message to fold. The cut falls between steps: the parts of
/// `to_message` up to and including `through_part` are folded, later parts
/// (the recent steps, reasoning included) stay. The messages from `kept_head`
/// up to `to_message` (the turn's prompt and notices) stay verbatim; the
/// marker goes right before `kept_head`, which is `to_message` itself when the
/// turn has no such head.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCut {
    /// Last folded part of `to_message`.
    pub through_part: PartId,
    /// First message kept after the marker.
    pub kept_head: MessageId,
}

/// Which compaction path produced a [`Event::ContextCompacted`] record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// Provider-native compaction (OpenAI `/responses/compact`).
    Native,
    /// Historical local model summarizer fallback. Decoded for replay and never emitted.
    LocalSummarizer,
    /// Historical deterministic archive of the folded prefix. Decoded for replay and never emitted.
    SnapCompact,
    /// Historical model-written handoff document. Decoded for replay and never emitted.
    Handoff,
    /// Kernel rewrite: whole steps before the current one are removed and
    /// replaced by a summary, initiated by the harness or the model.
    #[default]
    Rewrite,
}

impl CompactionStrategy {
    /// Stable snake_case wire name (the serde form): `native`,
    /// `local_summarizer`, `snap_compact`, `handoff`, or `rewrite`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::LocalSummarizer => "local_summarizer",
            Self::SnapCompact => "snap_compact",
            Self::Handoff => "handoff",
            Self::Rewrite => "rewrite",
        }
    }
}

impl Event {
    /// The session this event belongs to, if any.
    #[must_use]
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Event::SessionCreated { session, .. }
            | Event::InitialContextRecorded { session, .. }
            | Event::InitialContextInvalidated { session }
            | Event::SessionAgentModelOverrideSet { session, .. }
            | Event::SessionPermissionModeSet { session, .. }
            | Event::SessionTitled { session, .. }
            | Event::SessionMetadataSet { session, .. }
            | Event::SessionArchived { session, .. }
            | Event::SessionUnarchived { session, .. }
            | Event::SessionEphemeralSet { session, .. }
            | Event::AgentSwitched { session, .. }
            | Event::ModelSwitched { session, .. }
            | Event::CommandExecuted { session, .. }
            | Event::MessageStarted { session, .. }
            | Event::TurnBindingRecorded { session, .. }
            | Event::UserPromptContextRecorded { session, .. }
            | Event::MessageFinished { session, .. }
            | Event::PartDeleted { session, .. }
            | Event::StepStarted { session, .. }
            | Event::StepFinished { session, .. }
            | Event::TextStart { session, .. }
            | Event::TextDelta { session, .. }
            | Event::TextReplace { session, .. }
            | Event::TextEnd { session, .. }
            | Event::ReasoningStart { session, .. }
            | Event::ReasoningDelta { session, .. }
            | Event::ReasoningEnd { session, .. }
            | Event::ReasoningReplace { session, .. }
            | Event::ToolInputStart { session, .. }
            | Event::ToolInputDelta { session, .. }
            | Event::ToolCallRequested { session, .. }
            | Event::ToolResult { session, .. }
            | Event::ToolError { session, .. }
            | Event::ToolPartUpdated { session, .. }
            | Event::ContextCompacted { session, .. }
            | Event::SessionForked { session, .. }
            | Event::UsageRecorded { session, .. }
            | Event::TodosUpdated { session, .. }
            | Event::FilesChanged { session, .. }
            | Event::FilesObserved { session, .. }
            | Event::SessionReverted { session, .. }
            | Event::SessionUnreverted { session, .. }
            | Event::Extension { session, .. } => Some(*session),
            Event::Error { session, .. } => *session,
        }
    }
}

/// Metadata for one known path. Both values are null when the file is deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    /// Absolute path.
    pub path: String,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: Option<i64>,
    /// File size in bytes.
    pub size: Option<u64>,
}

/// An ordered, replayable event: the unit shipped over SSE and stored in the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Global log sequence, or `0` for live-only (never persisted) publishes.
    pub seq: EventSeq,
    /// Unix-epoch milliseconds when the envelope was produced.
    pub ts_millis: i64,
    /// Event payload.
    pub event: Event,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn turn_binding_round_trips_and_folds_only_generation_identity() {
        let session = SessionId::new();
        let message = MessageId::new();
        let binding = Event::TurnBindingRecorded {
            session,
            message,
            generation: ConfigGeneration::INITIAL,
            system_sha256: "a".repeat(64),
            tools_sha256: "b".repeat(64),
        };
        let encoded = serde_json::to_string(&binding).expect("encode turn binding");
        let decoded: Event = serde_json::from_str(&encoded).expect("decode turn binding");
        assert_eq!(decoded, binding);
        assert_eq!(decoded.session(), Some(session));

        let projection = crate::Projection::from_events(&[
            Envelope {
                seq: EventSeq(1),
                ts_millis: 1,
                event: Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            },
            Envelope {
                seq: EventSeq(2),
                ts_millis: 2,
                event: binding,
            },
        ]);
        let projected = projection
            .session
            .messages
            .first()
            .expect("projected assistant message");
        assert_eq!(projected.config_generation, Some(ConfigGeneration::INITIAL));
        // Registry contents stay out of the log; only their digests ride along.
        assert!(!encoded.contains("\"tools\""));
        assert!(!encoded.contains("skills"));
    }

    fn context_compacted(session: SessionId) -> Event {
        Event::ContextCompacted {
            session,
            message: MessageId::new(),
            strategy: CompactionStrategy::LocalSummarizer,
            from_message: MessageId::new(),
            to_message: MessageId::new(),
            folded_count: 34,
            input_tokens_est: 120_000,
            threshold: 100_000,
            step_cut: None,
        }
    }

    /// Wire round-trip and owning-session attribution across extension, fork,
    /// and rewrite events.
    #[test]
    fn events_round_trip_and_report_owning_session() {
        let root = SessionId::new();
        let parent = SessionId::new();
        let cases: Vec<(Event, SessionId)> = vec![
            (
                Event::Extension {
                    session: root,
                    bundle: "hya/workflow".to_string(),
                    kind: "run_started".to_string(),
                    payload: serde_json::json!({"run": "wfrun_123", "stage": "plan"}),
                },
                root,
            ),
            (
                Event::Extension {
                    session: parent,
                    bundle: "hya/multi-agent".to_string(),
                    kind: "member_spawned".to_string(),
                    payload: serde_json::json!({"handle": "main/worker-1"}),
                },
                parent,
            ),
            (context_compacted(root), root),
            (
                Event::SessionForked {
                    session: root,
                    source: parent,
                    before_message: None,
                },
                root,
            ),
            (
                Event::SessionForked {
                    session: root,
                    source: parent,
                    before_message: Some(MessageId::new()),
                },
                root,
            ),
        ];
        for (event, owner) in cases {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, back, "event must round-trip: {json}");
            assert_eq!(back.session(), Some(owner), "owning session for {json}");
        }
    }

    #[test]
    fn extension_event_uses_tagged_wire_format() {
        let session = SessionId::new();
        let event = Event::Extension {
            session,
            bundle: "hya/compaction".to_string(),
            kind: "context_evicted".to_string(),
            payload: serde_json::json!({"evicted_parts": 2}),
        };
        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(value["type"], "extension");
        assert_eq!(value["session"], session.to_string());
        assert_eq!(value["bundle"], "hya/compaction");
        assert_eq!(value["kind"], "context_evicted");
        assert_eq!(value["payload"]["evicted_parts"], 2);
        let back: Event = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, event);
    }

    #[test]
    fn compaction_strategy_round_trips_every_variant() {
        for strategy in [
            CompactionStrategy::Native,
            CompactionStrategy::LocalSummarizer,
            CompactionStrategy::SnapCompact,
            CompactionStrategy::Handoff,
            CompactionStrategy::Rewrite,
        ] {
            let json = serde_json::to_string(&strategy).expect("serialize");
            let back: CompactionStrategy = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(strategy, back, "strategy must round-trip: {json}");
            assert_eq!(
                json,
                format!("\"{}\"", strategy.as_str()),
                "as_str is the serde name"
            );
        }
        assert_eq!(
            serde_json::to_string(&CompactionStrategy::LocalSummarizer).expect("serialize"),
            "\"local_summarizer\"",
            "strategy uses snake_case on the wire"
        );
        assert_eq!(
            serde_json::to_string(&CompactionStrategy::SnapCompact).expect("serialize"),
            "\"snap_compact\"",
            "strategy uses snake_case on the wire"
        );
        assert_eq!(
            serde_json::to_string(&CompactionStrategy::Handoff).expect("serialize"),
            "\"handoff\"",
            "strategy uses snake_case on the wire"
        );
    }

    /// Without its marker row in the transcript (the reducer only moves that
    /// row), the record changes no state.
    #[test]
    fn context_compacted_without_its_marker_row_changes_no_state() {
        let session = SessionId::new();
        let base = [Envelope {
            seq: EventSeq(1),
            ts_millis: 1,
            event: Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("m"),
                workdir: "/w".to_string(),
                project: None,
                kind: crate::SessionKind::Project,
            },
        }];
        let mut with_record = base.to_vec();
        with_record.push(Envelope {
            seq: EventSeq(2),
            ts_millis: 2,
            event: context_compacted(session),
        });

        let before = crate::Projection::from_events(&base);
        let after = crate::Projection::from_events(&with_record);
        // `last_seq` advances for every appended event; the reduced state must not.
        assert_eq!(
            before.session, after.session,
            "only the marker row moves; there is none here"
        );
        assert_eq!(
            before.extensions, after.extensions,
            "no extension state may change"
        );
        assert_eq!(after.last_seq, 2, "the record still advances seq");
    }

    #[test]
    fn usage_recorded_round_trips_and_omits_side_call_fields() {
        let session = SessionId::new();
        let message = MessageId::new();
        let tokens = TokenUsage {
            input: 10,
            output: 7,
            reasoning: 0,
            cache_read: 3,
            cache_write: 2,
            reasoning_unknown: true,
        };
        let round = Event::UsageRecorded {
            session,
            message: Some(message),
            step: Some(1),
            model: ModelRef::new("anthropic/claude"),
            purpose: UsagePurpose::Turn,
            tokens,
        };
        let json = serde_json::to_string(&round).expect("serialize");
        assert!(json.contains(r#""type":"usage_recorded""#), "{json}");
        assert!(json.contains(r#""purpose":"turn""#), "{json}");
        assert!(json.contains(r#""reasoning_unknown":true"#), "{json}");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, round);
        assert_eq!(back.session(), Some(session));

        let side = Event::UsageRecorded {
            session,
            message: None,
            step: None,
            model: ModelRef::new("title-model"),
            purpose: UsagePurpose::Title,
            tokens: TokenUsage {
                reasoning_unknown: false,
                ..tokens
            },
        };
        let json = serde_json::to_string(&side).expect("serialize");
        assert!(!json.contains("\"message\""), "{json}");
        assert!(!json.contains("\"step\""), "{json}");
        assert!(!json.contains("reasoning_unknown"), "{json}");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, side);
    }

    #[test]
    fn message_finished_cause_round_trips_and_is_omitted_when_absent() {
        let session = SessionId::new();
        let message = MessageId::new();
        for cause in [
            FinishCause::UserCancel,
            FinishCause::Shutdown,
            FinishCause::LeaderFailed,
            FinishCause::Interrupted,
            FinishCause::ProviderError,
            FinishCause::Archived,
        ] {
            let finished = Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Cancelled,
                tokens: None,
                cause: Some(cause),
            };
            let json = serde_json::to_string(&finished).expect("serialize");
            assert!(json.contains(r#""cause":""#), "{json}");
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, finished);
        }
        let json = serde_json::to_string(&Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Error,
            tokens: None,
            cause: Some(FinishCause::ProviderError),
        })
        .expect("serialize");
        assert!(json.contains(r#""cause":"provider_error""#), "{json}");

        let plain = Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        };
        let json = serde_json::to_string(&plain).expect("serialize");
        assert!(
            !json.contains("cause"),
            "absent cause is not written: {json}"
        );
    }

    #[test]
    fn old_log_message_finished_without_cause_decodes() {
        let session = SessionId::new();
        let message = MessageId::new();
        // An old log row: exactly the pre-`cause` wire shape.
        let legacy = serde_json::json!({
            "type": "message_finished",
            "session": session,
            "message": message,
            "role": "assistant",
            "finish": "cancelled",
        });
        let decoded: Event = serde_json::from_value(legacy.clone()).expect("legacy finish decodes");
        assert_eq!(
            decoded,
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Cancelled,
                tokens: None,
                cause: None,
            }
        );
        // A cause written by a newer build decodes as `other`, never an error.
        let mut future = legacy;
        future["cause"] = serde_json::json!("power_loss");
        let decoded: Event = serde_json::from_value(future).expect("future cause decodes");
        assert!(matches!(
            decoded,
            Event::MessageFinished {
                cause: Some(FinishCause::Other),
                ..
            }
        ));
        // The projection keeps the cause next to the finish reason.
        let projection = crate::Projection::from_events(&[
            Envelope {
                seq: EventSeq(1),
                ts_millis: 1,
                event: Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            },
            Envelope {
                seq: EventSeq(2),
                ts_millis: 2,
                event: Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: FinishReason::Cancelled,
                    tokens: None,
                    cause: Some(FinishCause::Shutdown),
                },
            },
        ]);
        let projected = projection.session.messages.first().expect("message");
        assert_eq!(projected.finish, Some(FinishReason::Cancelled));
        assert_eq!(projected.cause, Some(FinishCause::Shutdown));
    }

    #[test]
    fn old_log_step_finished_without_error_decodes_and_error_round_trips() {
        let session = SessionId::new();
        let message = MessageId::new();
        let legacy = serde_json::json!({
            "type": "step_finished",
            "session": session,
            "message": message,
            "step": 2,
            "finish": "error",
        });
        let decoded: Event = serde_json::from_value(legacy).expect("legacy step_finished decodes");
        assert_eq!(
            decoded,
            Event::StepFinished {
                session,
                message,
                step: 2,
                finish: FinishReason::Error,
                error: None,
            }
        );
        let serialized = serde_json::to_value(&decoded).expect("serialize");
        assert!(serialized.get("error").is_none());

        let with_error = Event::StepFinished {
            session,
            message,
            step: 3,
            finish: FinishReason::Error,
            error: Some("provider stream interrupted: connection reset".to_string()),
        };
        let wire = serde_json::to_value(&with_error).expect("serialize with error");
        assert_eq!(
            wire.get("error").and_then(|v| v.as_str()),
            Some("provider stream interrupted: connection reset")
        );
        let back: Event = serde_json::from_value(wire).expect("round-trip");
        assert_eq!(back, with_error);
    }

    #[test]
    fn legacy_token_usage_decodes_with_known_flag_default() {
        let json = r#"{"input":5,"output":4,"reasoning":1,"cache_read":2,"cache_write":0}"#;
        let usage: TokenUsage = serde_json::from_str(json).expect("legacy usage decodes");
        assert!(!usage.reasoning_unknown);
        assert_eq!(usage.prompt(), 7);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod decode_contract_tests {
    use super::{Envelope, Event};

    #[test]
    fn retired_and_unknown_event_tags_are_decode_errors() {
        for tag in [
            "session_moved",
            "session_permission_set",
            "session_share_set",
            "session_share_cleared",
            "session_status",
            "message_deleted",
            "channel_left",
            "workflow_selected",
            "workflow_run_started",
            "workflow_stage_started",
            "workflow_stage_member_linked",
            "workflow_stage_route_outcome",
            "workflow_stage_finished",
            "workflow_run_finished",
            "member_spawned",
            "member_status_changed",
            "member_finished",
            "agent_registered",
            "agent_activity_changed",
            "resident_work_started",
            "resident_work_finished",
            "agent_heartbeat",
            "mail_sent",
            "channel_joined",
            "channel_created",
            "subagent_reported",
            "handoff_committed",
            "agent_archived",
            "mail_consumed",
            "agent_restarted",
            "context_evicted",
            "context_status",
            "future_event",
        ] {
            let payload = format!(r#"{{"type":"{tag}"}}"#);
            let error = serde_json::from_str::<Event>(&payload).unwrap_err();
            assert!(error.to_string().contains(tag), "{error}");
            let envelope = format!(r#"{{"seq":7,"ts_millis":1,"event":{payload}}}"#);
            assert!(serde_json::from_str::<Envelope>(&envelope).is_err());
        }
    }
}
