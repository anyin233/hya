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

use crate::ids::{
    ActorEpoch, ConfigGeneration, EventSeq, MemberId, MessageId, OwnerRunId, PartId, ProjectId,
    SessionId, ToolCallId, WorkflowRunId,
};
use crate::mail::{ChannelKind, MailEndpoint, MailKind};
use crate::message::{
    FinishCause, FinishReason, MemberRunStatus, Role, RosterStatus, SubagentMode, TokenUsage,
    ToolPartState, UsagePurpose,
};
use crate::model::{AgentName, ModelRef, ToolName};
use crate::tokens::{TokenAccountingMode, TokenSource};
use crate::workflow::{
    WorkflowIdentity, WorkflowMemberRole, WorkflowRunStatus, WorkflowStagePlan, WorkflowStageStatus,
};

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

    /// Parse a wire/storage label; `None` for anything but `project` or
    /// `temporary`.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "project" => Some(Self::Project),
            "temporary" => Some(Self::Temporary),
            _ => None,
        }
    }
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
    /// wins. Older binaries fold this variant as `Unknown`.
    SessionPermissionModeSet {
        /// Root Session whose descendant tree uses this mode.
        session: SessionId,
        /// Mode identifier (`manual`, `yolo`, or `<bundle-id>/<mode-id>`).
        mode: String,
    },
    /// Session workdir changed.
    SessionMoved {
        /// Session this event belongs to.
        session: SessionId,
        /// New absolute workdir.
        workdir: String,
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
    /// Session permission rule list replaced (does not merge).
    SessionPermissionSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Full permission rule list after the set.
        permission: Vec<serde_json::Value>,
    },
    /// A root session was archived (hidden from default session lists).
    ///
    /// Folds `SessionProjection.archived` to the stamp. Archiving never
    /// cancels a running turn. A zero stamp (written by the deleted Compat
    /// surface to clear the archive) folds as not archived.
    SessionArchived {
        /// Session this event belongs to.
        session: SessionId,
        /// Unix epoch milliseconds when the session was archived.
        archived: serde_json::Number,
    },
    /// A root session left the archive: explicitly, or implicitly because a
    /// new prompt or shell turn was admitted on it. Folds
    /// `SessionProjection.archived` to `None`. An older binary folds it as
    /// `Unknown`.
    SessionUnarchived {
        /// Session this event belongs to.
        session: SessionId,
    },
    /// A root session was marked ephemeral (`true`: created for a client
    /// before the user asked for one, e.g. the TUI's session on connect) or
    /// kept (`false`: a fork was taken from it). The server deletes an
    /// ephemeral session once it is still unused and no client watches it.
    /// Folds `SessionProjection.ephemeral`; the session's first message, a
    /// title, or an archive also clear it for good. An older binary folds it
    /// as `Unknown` (the session is then simply kept).
    SessionEphemeralSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Whether the session is (still) ephemeral.
        ephemeral: bool,
    },
    /// Share URL recorded for the session.
    SessionShareSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Public share URL.
        url: String,
    },
    /// Share URL cleared (reducer sets share to `None`).
    SessionShareCleared {
        /// Session this event belongs to.
        session: SessionId,
    },
    /// Active agent switched for the session.
    AgentSwitched {
        /// Session this event belongs to.
        session: SessionId,
        /// Optional transcript anchor for when the switch occurred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        /// New agent name.
        agent: AgentName,
    },
    /// Active model switched for the session.
    ModelSwitched {
        /// Session this event belongs to.
        session: SessionId,
        /// Optional transcript anchor for when the switch occurred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        /// New model reference.
        model: ModelRef,
    },
    /// Free-form status ping; reducer no-op (compat `session.status` bridge).
    SessionStatus {
        /// Session this event belongs to.
        session: SessionId,
        /// Opaque status payload for live UIs.
        status: serde_json::Value,
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
    /// Select an exact compiled Workflow identity; the last event wins.
    WorkflowSelected {
        /// Session whose control state changes.
        session: SessionId,
        /// Stable source/name/revision identity.
        workflow: WorkflowIdentity,
    },
    /// Begin one durable Workflow run and capture its declaration-ordered plan.
    WorkflowRunStarted {
        /// Session that owns the Workflow control state.
        session: SessionId,
        /// Stable run identity.
        run: WorkflowRunId,
        /// Exact compiled identity executed by this run.
        workflow: WorkflowIdentity,
        /// Canonical hash of source, caller, input pairs, and bound runtime.
        request_hash: String,
        /// Process owner that admitted this run.
        owner: OwnerRunId,
        /// Stable display/provenance plan; never directives, inputs, or outputs.
        stages: Vec<WorkflowStagePlan>,
    },
    /// Mark one compiled Stage as active.
    WorkflowStageStarted {
        /// Session that owns the Workflow run.
        session: SessionId,
        /// Run that owns this Stage activation.
        run: WorkflowRunId,
        /// Compiled Stage id.
        stage: String,
    },
    /// Link one canonical Member reference to a Workflow Stage.
    WorkflowStageMemberLinked {
        /// Session that owns the Workflow run.
        session: SessionId,
        /// Run that owns this member.
        run: WorkflowRunId,
        /// Compiled Stage id.
        stage: String,
        /// Member identity in the authoritative Session member projection.
        member: MemberId,
        /// Worker or independent verifier role.
        role: WorkflowMemberRole,
        /// Zero-based activation iteration.
        iteration: u32,
    },
    /// Record one finalized candidate selection for an explicit Stage route.
    ///
    /// This is one bounded observation per provider stream group; it carries
    /// no provider text, credentials, prompts, or response content.
    WorkflowStageRouteOutcome {
        /// Owning root Session log.
        session: SessionId,
        /// Workflow run containing the Stage.
        run: WorkflowRunId,
        /// Compiled Stage id.
        stage: String,
        /// Canonical Session member reference.
        member: MemberId,
        /// Worker or independent verifier route.
        role: WorkflowMemberRole,
        /// Zero-based loop activation iteration.
        iteration: u32,
        /// Assistant/provider stream-group index.
        step: u32,
        /// Declaration-order candidate index selected or finally attempted.
        candidate_index: u32,
        /// Base model identity without a Workflow variant suffix.
        model: ModelRef,
        /// Required canonical effort label (`none` means Off).
        reasoning: String,
        /// Stable provider/activation failure class.
        failure_class: WorkflowRouteFailureClass,
    },
    /// Finish one Workflow Stage with a terminal status.
    WorkflowStageFinished {
        /// Session that owns the Workflow run.
        session: SessionId,
        /// Run that owns this Stage activation.
        run: WorkflowRunId,
        /// Compiled Stage id.
        stage: String,
        /// Terminal Stage status.
        status: WorkflowStageStatus,
    },
    /// Finish one Workflow run with a terminal status.
    WorkflowRunFinished {
        /// Session that owns the Workflow run.
        session: SessionId,
        /// Run being terminalized.
        run: WorkflowRunId,
        /// Terminal run status.
        status: WorkflowRunStatus,
        /// Bounded terminal error detail, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
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
    /// Records which immutable runtime snapshot (`ConfigGeneration`) ran this assistant turn.
    TurnBindingRecorded {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message the binding applies to.
        message: MessageId,
        /// Lightweight generation identity (registry contents stay outside the log).
        generation: ConfigGeneration,
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
        /// recovery, provider failure). Absent on model-ended messages and on
        /// logs written before the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<FinishCause>,
    },
    /// Removes a whole message from the projected view.
    MessageDeleted {
        /// Session this event belongs to.
        session: SessionId,
        /// Message to drop.
        message: MessageId,
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
    /// Provider round started; reducer no-op (UI / step markers).
    StepStarted {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message owning the round.
        message: MessageId,
        /// Zero-based round index within the turn.
        step: u32,
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
    /// Raw argument JSON stream; reducer no-op (compat may forward as pending raw).
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

    // -------- subagent (member) lifecycle --------
    // These attach to the PARENT (`session`) so they live in the parent's log and
    // stream with it. They carry only bounded metadata + a short summary — never a
    // child transcript — so observers can render a live agent tree cheaply.
    /// Member spawned on the **parent** log (status → Spawning); never carries child transcript.
    MemberSpawned {
        /// Parent session log this event is appended to.
        session: SessionId,
        /// Member id within the parent tree.
        member: MemberId,
        /// Child session when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<SessionId>,
        /// Subagent type / agent name for the spawn.
        subagent_type: AgentName,
        /// Short human description of the task.
        description: String,
        /// Depth in the subagent tree (root children are 1).
        depth: u32,
        /// Verbatim parent directive that defines this member's purpose.
        ///
        /// Recorded on the edge because the child's first user message is not a
        /// reliable substitute: a resumed session receives the directive as a
        /// later message, and a resident agent also receives mail as user
        /// prompts. Offline viewers summarize this; the engine never does.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        directive: String,
        /// Tool call that caused this spawn, when it came from one.
        ///
        /// `None` for members the resident supervisor starts without a tool call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call: Option<ToolCallId>,
    },
    /// Member status update on the parent log.
    MemberStatusChanged {
        /// Parent session log.
        session: SessionId,
        /// Member being updated.
        member: MemberId,
        /// New run status.
        status: MemberRunStatus,
    },
    /// Member finished with a **bounded** summary string (not the child transcript).
    MemberFinished {
        /// Parent session log.
        session: SessionId,
        /// Member being finished.
        member: MemberId,
        /// Terminal member status.
        status: MemberRunStatus,
        /// Bounded summary for the parent/TUI.
        summary: String,
        /// Optional child session id when known at finish.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<SessionId>,
    },

    // -------- event-sourced mailbox & channels (ADR-0001) --------
    // Team-scoped comms. Every variant is appended to the TEAM-ROOT session's log
    // (`session` = the root of the team tree) so a single replay reconstructs the
    // whole team's inboxes/channels/roster, and the live bus carries them to the
    // TUI for free. Additive variants — older binaries fold them via `Unknown`.
    /// Bind a team member's session to its stable handle within its unit.
    /// `agent_session` is the registered agent's own session; `session` is the
    /// team-root log the binding is recorded in.
    AgentRegistered {
        /// Team-root log session.
        session: SessionId,
        /// Agent's own session id.
        agent_session: SessionId,
        /// The agent's **leaf** name — its short name inside its unit, not a
        /// path. The reducer joins it to `parent` to form the canonical handle.
        handle: String,
        /// Canonical path of this agent's parent, e.g. `main/lead-1`.
        ///
        /// `None` means the team root. Combined with `skip_serializing_if`, a
        /// root registration stays byte-identical on the wire, and a pre-scoping
        /// log — which has no `parent` anywhere — folds as one flat unit under
        /// `main`, preserving its original behavior (task 08-07, AC8).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        /// Declared agent type for the roster.
        #[serde(default)]
        agent_type: AgentName,
        /// Transient vs resident scheduling (ADR-0002). `#[serde(default)]` so logs
        /// predating Phase 4 replay every member as transient.
        #[serde(default)]
        mode: SubagentMode,
    },
    /// A team member's live activity changed (idle ⇄ busy, or a terminal
    /// done/failed), optionally updating its short current-task label. Appended to
    /// the TEAM-ROOT log by the resident supervisor so the roster status column
    /// and quiescence view replay for free.
    AgentActivityChanged {
        /// Team-root log session.
        session: SessionId,
        /// Roster handle being updated.
        handle: String,
        /// New live activity.
        status: RosterStatus,
        /// Optional short task label; `Some("resident stopped")` is an explicit-stop sentinel.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_task: Option<String>,
    },
    /// Durable boundary between queued resident work and a turn that may dispatch
    /// tools, children, or provider requests. Appended before those effects.
    ResidentWorkStarted {
        /// Team-root log session.
        session: SessionId,
        /// Resident actor session whose work is starting.
        actor_session: SessionId,
        /// Roster handle of the actor.
        handle: String,
        /// Actor epoch for this incarnation.
        epoch: ActorEpoch,
        /// Inbox length covered by this coalesced resident turn.
        inbox_through: u64,
    },
    /// Harness-observed liveness for one resident (ADR-0002). Emitted ONLY by
    /// the resident supervisor — never by the agent (no tool surface can) —
    /// derived from real engine events the harness observed on the bus (tool
    /// results, text deltas, turn boundaries). Folds onto the roster row as a
    /// max, so the parent can tell a busy child that is progressing from one
    /// that has stalled.
    AgentHeartbeat {
        /// Team-root log session.
        session: SessionId,
        /// Roster handle whose actor showed harness-observed activity.
        handle: String,
        /// Unix-epoch milliseconds of the observed activity; the fold keeps
        /// the max, so stale values never regress liveness.
        heartbeat_ms: u64,
    },
    /// A message from one handle to another handle or a `#channel`. Channel sends
    /// fan out to every current eligible subscriber in the deterministic reducer, so no
    /// recipient set is baked into the event.
    MailSent {
        /// Team-root log session.
        session: SessionId,
        /// Sender handle.
        from: String,
        /// Direct handle or channel endpoint.
        to: MailEndpoint,
        /// Message intent (default chatter vs announcement).
        #[serde(default)]
        kind: MailKind,
        /// Message body text.
        body: String,
    },
    /// A handle subscribed to a channel; subsequent channel mail reaches it.
    ChannelJoined {
        /// Team-root log session.
        session: SessionId,
        /// Channel name without leading `#`.
        channel: String,
        /// Member handle joining.
        member: String,
    },
    /// A handle unsubscribed from a channel.
    ChannelLeft {
        /// Team-root log session.
        session: SessionId,
        /// Channel name without leading `#`.
        channel: String,
        /// Member handle leaving.
        member: String,
    },

    // -------- unified orchestration lifecycle (ADR-0015/0016) --------
    /// A channel was minted as an event fact: group channels carry the leader
    /// plus its direct reports; DM channels carry the vertical pair. The id is
    /// the canonical channel key (`announce-{8}` / `DM-{8}`).
    ChannelCreated {
        /// Team-root log session.
        session: SessionId,
        /// Channel key without the leading `#`.
        channel: String,
        /// Group broadcast vs DM pair.
        #[serde(default)]
        kind: ChannelKind,
        /// Owning unit path for group channels (`main/lead-1`); `None` for DM
        /// pairs. Durable unit→channel mapping so leader-only posting and
        /// broadcast resolution replay without derivation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        /// Canonical handles of the founding members.
        #[serde(default)]
        members: Vec<String>,
    },
    /// Terminal task report from a subagent to its parent. Appended to the
    /// PARENT log; the engine also delivers the report as mail on the
    /// parent-child DM channel.
    SubagentReported {
        /// Parent session log.
        session: SessionId,
        /// Member id within the parent tree.
        member: MemberId,
        /// Reporting child session.
        child: SessionId,
        /// Reporting agent's canonical handle (team-root key).
        handle: String,
        /// Terminal outcome.
        outcome: ReportOutcome,
        /// Bounded result text for the parent.
        report: String,
    },
    /// The state-only handoff document that survives an archive. Appended to
    /// the CHILD's own log; `generation` counts episodes and each document
    /// anchors on its predecessor.
    HandoffCommitted {
        /// Child session log.
        session: SessionId,
        /// Handle within the team (cross-log discovery).
        handle: String,
        /// Episode generation of this document.
        generation: u32,
        /// Full handoff document (six state-only sections).
        doc: String,
        /// A deterministic projection-derived fallback produced this document.
        #[serde(default)]
        degraded: bool,
    },
    /// An agent left the live roster: the sole archive marker. Removes the
    /// roster row and group-channel membership; the DM channel persists as the
    /// revival address.
    AgentArchived {
        /// Team-root log session.
        session: SessionId,
        /// Archived agent's canonical handle.
        handle: String,
        /// Archived agent's session.
        child: SessionId,
        /// Why the agent archived.
        reason: ArchiveReason,
    },
    /// In-turn mail consumption (steer): unread mail surfaced to the agent
    /// inside a tool result advances the durable inbox cursor, so the report
    /// gate and later turns never re-inject or re-block on mail the agent has
    /// already seen mid-turn.
    MailConsumed {
        /// Team-root log session.
        session: SessionId,
        /// Consuming roster handle.
        handle: String,
        /// Inbox length covered by this consumption.
        through: u64,
    },

    /// An archived agent was revived by a downward DM. Paired with an
    /// `AgentRegistered` upsert in the same transaction.
    AgentRestarted {
        /// Team-root log session.
        session: SessionId,
        /// Revived agent's canonical handle.
        handle: String,
        /// Revived agent's session.
        child: SessionId,
        /// New actor epoch for this incarnation.
        epoch: ActorEpoch,
    },

    // -------- context observability --------
    /// A compaction folded part of this session's transcript.
    ///
    /// Appended to the log of the session that compacted; there is no parent
    /// mirror, because `event_log.seq` already orders every agent's events on one
    /// global timeline. Reducer no-op: this is a record, not a state transition.
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

    /// Stale tool outputs were dropped from this turn's request to fit the window.
    ///
    /// Cheaper than summarizing: the model keeps every tool call and its input,
    /// and loses only old outputs it can re-fetch. Emitted when eviction alone
    /// brought the transcript under the threshold, so no summarization ran.
    ///
    /// **Request-local.** The event log still holds every tool output in full, so
    /// an offline viewer reconstructs the true trajectory; only what was *sent*
    /// this turn was reduced. Reducer no-op.
    ContextEvicted {
        /// Session whose request was reduced.
        session: SessionId,
        /// Number of tool-output parts replaced with a size notice.
        evicted_parts: u32,
        /// Token count before eviction.
        tokens_before: u64,
        /// Token count after eviction.
        tokens_after: u64,
        /// Threshold in force when it ran.
        threshold: u64,
    },

    /// Window occupancy for one model request, with how the figure was derived.
    ///
    /// Emitted once per streaming round, after any compaction ladder walk, so
    /// the recorded number is the occupancy actually sent. This is the wire
    /// surface of token accounting: the count, whether it was provider-anchored
    /// or locally estimated, the accounting mode in force, and the resolved
    /// threshold it was judged against. The projection keeps the latest per
    /// session.
    ContextStatus {
        /// Session whose request was measured.
        session: SessionId,
        /// Tokens the request is believed to occupy.
        tokens: u64,
        /// Whether the count was provider-anchored or locally estimated.
        source: TokenSource,
        /// Accounting mode in force for the round.
        mode: TokenAccountingMode,
        /// Resolved compaction threshold the count was judged against.
        threshold: u64,
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
    /// that have records. An older binary folds this variant as `Unknown`.
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
    /// Appended by the engine right after the `ToolResult` of a todo tool
    /// whose result carries a list different from the folded one (reads that
    /// change nothing append nothing). Folds into `SessionProjection.todos`.
    /// Sessions that predate this event have none; their list is read from
    /// the todo tools' results instead. An older binary folds it as `Unknown`.
    TodosUpdated {
        /// Session whose todo list changed.
        session: SessionId,
        /// Full replacement list, in order.
        todos: Vec<crate::TodoItem>,
    },

    // -------- file snapshots and revert --------
    /// Files one tool call changed, each with its content before the change.
    ///
    /// Appended by the engine right after the call's `ToolResult` /
    /// `ToolError` when the call changed at least one file (write, edit,
    /// apply_patch, and bash inside a git work tree). Contents live in the
    /// store's per-session blob table; the event carries only paths and blob
    /// keys. Folds onto `MessageProjection.file_changes` of `message` and is
    /// what a revert restores from. An older binary folds it as `Unknown`.
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

    /// The transcript was reverted to just before user message `message`.
    ///
    /// Folds: `message` and every later message move from
    /// `SessionProjection.messages` to `SessionProjection.revert.hidden`.
    /// Recorded after the engine restored the files the hidden turns changed
    /// (`files`). A revert while one is pending extends it further back. The
    /// next `MessageStarted` commits the pending revert (the hidden messages
    /// are dropped for good). An older binary folds it as `Unknown`.
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
    /// A no-op fold when no revert is pending. An older binary folds it as
    /// `Unknown`.
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
        /// for errors not tied to a message and in logs written before 0.41.0.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed_message: Option<MessageId>,
    },

    /// Forward-compatibility catch-all: any event whose `type` tag is not one of
    /// the variants above deserializes here instead of failing. This lets an older
    /// binary replay a log (or a client decode a stream) that contains newer event
    /// variants without erroring. NOTE: this is a unit variant, so the original
    /// payload is dropped — code that must forward unknown events losslessly should
    /// decode the raw JSON (`serde_json::Value`) at the boundary rather than relying
    /// on this round-tripping.
    #[serde(other)]
    Unknown,
}
/// Terminal outcome of a subagent's task report (ADR-0015).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportOutcome {
    /// Task completed successfully.
    Done,
    /// Task failed; the report carries the blocker.
    Failed,
}

/// Why an agent left the live roster (ADR-0015). Archive is the sole exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveReason {
    /// Terminal report accepted (model-issued or engine-synthesized).
    Reported,
    /// Legacy (before 0.41.0): parent `kill` tool. Kept so old logs decode.
    Killed,
    /// Root-turn teardown force-archive.
    RootTeardown,
    /// The parent (or an ancestor) archived the member with the `archive`
    /// tool; any in-flight turn was cancelled first.
    ArchivedByParent,
    /// A graceful stop (end of a one-shot run, SIGINT/SIGTERM, `serve`
    /// shutdown) archived the member so a later run can wake it with mail.
    Shutdown,
}

/// Stable, bounded classification for one Workflow route outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRouteFailureClass {
    /// The selected candidate succeeded without a fallback advance.
    None,
    /// Retryable transport failure before a stream was established.
    Transport,
    /// Upstream rate limiting before a stream was established.
    RateLimited,
    /// Retryable upstream server failure before a stream was established.
    Server,
    /// No provider route claimed the candidate model.
    UnknownModel,
    /// Authentication or authorization failure.
    Auth,
    /// Provider capability or request incompatibility.
    Incompatible,
    /// Non-retryable HTTP/provider response failure.
    Http,
    /// Malformed or truncated provider stream.
    Decode,
    /// Internal invariant, store, or claim failure after an attempt started.
    Internal,
    /// Sidecar loss or task abort after an attempt started.
    Aborted,
    /// Explicit Workflow-owned activation cancellation.
    Cancelled,
}

/// Durable fields for one finalized explicit Workflow route stream group.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStageRouteOutcome {
    /// Owning root Session log.
    pub session: SessionId,
    /// Workflow run containing the Stage.
    pub run: WorkflowRunId,
    /// Compiled Stage id.
    pub stage: String,
    /// Canonical Session member reference.
    pub member: MemberId,
    /// Worker or independent verifier route.
    pub role: WorkflowMemberRole,
    /// Zero-based loop activation iteration.
    pub iteration: u32,
    /// Assistant/provider stream-group index.
    pub step: u32,
    /// Declaration-order candidate index selected or finally attempted.
    pub candidate_index: u32,
    /// Base model identity without a Workflow variant suffix.
    pub model: ModelRef,
    /// Required canonical effort label (`none` means Off).
    pub reasoning: String,
    /// Stable provider/activation failure class.
    pub failure_class: WorkflowRouteFailureClass,
}
fn default_step_finish_reason() -> FinishReason {
    FinishReason::Stop
}

/// Which compaction path produced a [`Event::ContextCompacted`] record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// Provider-native compaction (OpenAI `/responses/compact`).
    Native,
    /// Local model summarizer fallback.
    #[default]
    LocalSummarizer,
    /// Local deterministic dense archive of the folded prefix, no model call.
    SnapCompact,
    /// Model-written handoff document over the verbatim transcript.
    Handoff,
}

impl CompactionStrategy {
    /// Stable snake_case wire name (the serde form): `native`,
    /// `local_summarizer`, `snap_compact`, or `handoff`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::LocalSummarizer => "local_summarizer",
            Self::SnapCompact => "snap_compact",
            Self::Handoff => "handoff",
        }
    }
}

impl Event {
    /// The session this event belongs to, if any.
    #[must_use]
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Event::SessionCreated { session, .. }
            | Event::SessionAgentModelOverrideSet { session, .. }
            | Event::SessionPermissionModeSet { session, .. }
            | Event::SessionMoved { session, .. }
            | Event::SessionTitled { session, .. }
            | Event::SessionMetadataSet { session, .. }
            | Event::SessionPermissionSet { session, .. }
            | Event::SessionArchived { session, .. }
            | Event::SessionUnarchived { session, .. }
            | Event::SessionEphemeralSet { session, .. }
            | Event::SessionShareSet { session, .. }
            | Event::SessionShareCleared { session, .. }
            | Event::AgentSwitched { session, .. }
            | Event::ModelSwitched { session, .. }
            | Event::SessionStatus { session, .. }
            | Event::CommandExecuted { session, .. }
            | Event::WorkflowSelected { session, .. }
            | Event::WorkflowRunStarted { session, .. }
            | Event::WorkflowStageStarted { session, .. }
            | Event::WorkflowStageMemberLinked { session, .. }
            | Event::WorkflowStageRouteOutcome { session, .. }
            | Event::WorkflowStageFinished { session, .. }
            | Event::WorkflowRunFinished { session, .. }
            | Event::MessageStarted { session, .. }
            | Event::TurnBindingRecorded { session, .. }
            | Event::UserPromptContextRecorded { session, .. }
            | Event::MessageFinished { session, .. }
            | Event::MessageDeleted { session, .. }
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
            | Event::ToolError { session, .. } => Some(*session),
            Event::ToolPartUpdated { session, .. } => Some(*session),
            Event::MemberSpawned { session, .. }
            | Event::MemberStatusChanged { session, .. }
            | Event::MemberFinished { session, .. } => Some(*session),
            Event::AgentRegistered { session, .. }
            | Event::AgentActivityChanged { session, .. }
            | Event::ResidentWorkStarted { session, .. }
            | Event::AgentHeartbeat { session, .. }
            | Event::MailSent { session, .. }
            | Event::ChannelJoined { session, .. }
            | Event::ChannelLeft { session, .. }
            | Event::ChannelCreated { session, .. }
            | Event::SubagentReported { session, .. }
            | Event::HandoffCommitted { session, .. }
            | Event::AgentArchived { session, .. }
            | Event::AgentRestarted { session, .. }
            | Event::MailConsumed { session, .. } => Some(*session),
            Event::ContextCompacted { session, .. }
            | Event::SessionForked { session, .. }
            | Event::ContextEvicted { session, .. }
            | Event::ContextStatus { session, .. }
            | Event::UsageRecorded { session, .. }
            | Event::TodosUpdated { session, .. }
            | Event::FilesChanged { session, .. }
            | Event::SessionReverted { session, .. }
            | Event::SessionUnreverted { session, .. } => Some(*session),
            Event::Error { session, .. } => *session,
            Event::Unknown => None,
        }
    }
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
        assert!(!encoded.contains("tools"));
        assert!(!encoded.contains("skills"));
    }

    #[test]
    fn unknown_event_type_deserializes_to_unknown() {
        // A future/unknown `type` must not fail deserialization: it maps to
        // Event::Unknown so old binaries can replay logs with newer variants.
        let json = r#"{"type":"totally_made_up_future_event","session":"ses_x","x":1}"#;
        let event: Event = serde_json::from_str(json).expect("unknown type must decode");
        assert_eq!(event, Event::Unknown);
        assert_eq!(event.session(), None);

        // A known variant still decodes to its proper variant.
        let known =
            r#"{"type":"session_share_cleared","session":"ses_00000000000000000000000000000001"}"#;
        let event: Event = serde_json::from_str(known).expect("known type decodes");
        assert!(matches!(event, Event::SessionShareCleared { .. }));

        // Envelope carrying an unknown event also decodes.
        let env_json = format!(r#"{{"seq":7,"ts_millis":1,"event":{json}}}"#);
        let env: Envelope = serde_json::from_str(&env_json).expect("envelope decodes");
        assert_eq!(env.event, Event::Unknown);
    }

    #[test]
    fn mailbox_events_round_trip_through_json() {
        let root = SessionId::new();
        let agent = SessionId::new();
        for event in [
            Event::AgentRegistered {
                session: root,
                agent_session: agent,
                handle: "reviewer-3".to_string(),
                parent: None,
                agent_type: AgentName::new("reviewer"),
                mode: SubagentMode::Resident,
            },
            Event::AgentActivityChanged {
                session: root,
                handle: "reviewer-3".to_string(),
                status: RosterStatus::Busy,
                current_task: Some("reviewing".to_string()),
            },
            Event::ResidentWorkStarted {
                session: root,
                actor_session: agent,
                handle: "reviewer-3".to_string(),
                epoch: ActorEpoch::INITIAL,
                inbox_through: 2,
            },
            Event::MailSent {
                session: root,
                from: "main".to_string(),
                to: MailEndpoint::Channel("build".to_string()),
                kind: MailKind::Announcement,
                body: "ship it".to_string(),
            },
            Event::MailSent {
                session: root,
                from: "reviewer-1".to_string(),
                to: MailEndpoint::Handle("reviewer-2".to_string()),
                kind: MailKind::Message,
                body: "hi".to_string(),
            },
            Event::ChannelJoined {
                session: root,
                channel: "build".to_string(),
                member: "reviewer-1".to_string(),
            },
            Event::ChannelLeft {
                session: root,
                channel: "build".to_string(),
                member: "reviewer-1".to_string(),
            },
        ] {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, back, "mailbox event must round-trip: {json}");
        }
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
        }
    }

    #[test]
    fn context_compacted_round_trips_and_reports_its_session() {
        let session = SessionId::new();
        let event = context_compacted(session);
        let json = serde_json::to_string(&event).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event, back, "context_compacted must round-trip: {json}");
        assert_eq!(back.session(), Some(session));
    }

    #[test]
    fn compaction_strategy_round_trips_every_variant() {
        for strategy in [
            CompactionStrategy::Native,
            CompactionStrategy::LocalSummarizer,
            CompactionStrategy::SnapCompact,
            CompactionStrategy::Handoff,
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

    #[test]
    fn context_compacted_is_a_record_and_does_not_change_the_projection() {
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
            "ContextCompacted is an observability record, not a state transition"
        );
        assert_eq!(before.team, after.team, "no team state may change");
        assert_eq!(after.last_seq, 2, "the record still advances seq");
    }

    /// AC8: a log written before this task must still replay. `MemberSpawned`
    /// predates `directive` / `tool_call`, so both must default rather than fail.
    #[test]
    fn pre_change_member_spawned_still_decodes_and_folds() {
        let session = SessionId::new();
        let child = SessionId::new();
        // Both new fields are `skip_serializing_if`, so an event with empty
        // values encodes byte-for-byte like a log written before this task.
        let legacy = serde_json::to_string(&Event::MemberSpawned {
            session,
            member: MemberId::new(),
            child: Some(child),
            subagent_type: AgentName::new("explore"),
            description: "scan".to_string(),
            depth: 1,
            directive: String::new(),
            tool_call: None,
        })
        .expect("serialize");
        assert!(
            !legacy.contains("directive") && !legacy.contains("tool_call"),
            "empty additions must not appear on the wire: {legacy}"
        );
        let decoded: Event =
            serde_json::from_str(&legacy).expect("a pre-change log must still decode");
        let Event::MemberSpawned {
            directive,
            tool_call,
            ..
        } = &decoded
        else {
            panic!("expected MemberSpawned, got {decoded:?}");
        };
        assert!(directive.is_empty(), "absent directive defaults to empty");
        assert!(tool_call.is_none(), "absent tool call defaults to none");

        // And it still folds into a member row exactly as before.
        let projection = crate::Projection::from_events(&[Envelope {
            seq: EventSeq(1),
            ts_millis: 1,
            event: decoded,
        }]);
        assert_eq!(projection.session.members.len(), 1);
        assert_eq!(projection.session.members[0].child, Some(child));
        assert!(projection.session.members[0].directive.is_empty());
    }

    #[test]
    fn session_forked_round_trips_with_and_without_a_cut_point() {
        let session = SessionId::new();
        for before_message in [None, Some(MessageId::new())] {
            let event = Event::SessionForked {
                session,
                source: SessionId::new(),
                before_message,
            };
            let json = serde_json::to_string(&event).expect("serialize");
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, back, "session_forked must round-trip: {json}");
            assert_eq!(back.session(), Some(session));
        }
    }

    #[test]
    fn context_evicted_round_trips_and_is_a_record_only() {
        let session = SessionId::new();
        let event = Event::ContextEvicted {
            session,
            evicted_parts: 3,
            tokens_before: 5000,
            tokens_after: 1200,
            threshold: 4000,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event, back, "context_evicted must round-trip: {json}");
        assert_eq!(back.session(), Some(session));

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
            event: back,
        });
        let before = crate::Projection::from_events(&base);
        let after = crate::Projection::from_events(&with_record);
        assert_eq!(
            before.session, after.session,
            "eviction is request-local; it must not change reduced state"
        );
    }

    #[test]
    fn unknown_event_type_still_folds_for_older_binaries() {
        // Forward compatibility: a binary predating ContextCompacted must not
        // fail to replay a log that contains it.
        let decoded: Event = serde_json::from_str(r#"{"type":"some_future_event"}"#)
            .expect("unknown event type must decode as Unknown");
        assert_eq!(decoded, Event::Unknown);
        assert_eq!(decoded.session(), None);
    }

    /// A pre-scoping `agent_registered` payload carries no `parent` key at all.
    /// It must still deserialize, with `parent` defaulting to `None` — the whole
    /// basis of legacy-log replay (task 08-07, AC8).
    #[test]
    fn agent_registered_without_parent_deserializes_as_root_child() {
        let root = SessionId::new();
        let agent = SessionId::new();
        let legacy = format!(
            r#"{{"type":"agent_registered","session":"{root}","agent_session":"{agent}",
                 "handle":"reviewer-1","agent_type":"reviewer","mode":"resident"}}"#
        );
        let event: Event = serde_json::from_str(&legacy).expect("legacy payload deserializes");
        match event {
            Event::AgentRegistered { parent, handle, .. } => {
                assert_eq!(parent, None, "absent parent means the team root");
                assert_eq!(handle, "reviewer-1");
            }
            other => panic!("expected AgentRegistered, got {other:?}"),
        }
    }

    /// Orchestration events (ADR-0015/0016) round-trip and report the right
    /// owning session: root log for channel/archive/restart, parent log for the
    /// report, child log for the handoff.
    #[test]
    fn orchestration_events_round_trip_through_json() {
        let root = SessionId::new();
        let parent = SessionId::new();
        let child = SessionId::new();
        let cases: Vec<(Event, SessionId)> = vec![
            (
                Event::ChannelCreated {
                    session: root,
                    channel: "announce-aB12Cd34".to_string(),
                    kind: ChannelKind::Group,
                    unit: None,
                    members: vec!["main".to_string()],
                },
                root,
            ),
            (
                Event::ChannelCreated {
                    session: root,
                    channel: "DM-aB12Cd34".to_string(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec!["main".to_string(), "main/lead-1".to_string()],
                },
                root,
            ),
            (
                Event::SubagentReported {
                    session: parent,
                    member: MemberId::new(),
                    child,
                    handle: "main/lead-1".to_string(),
                    outcome: ReportOutcome::Done,
                    report: "shipped".to_string(),
                },
                parent,
            ),
            (
                Event::SubagentReported {
                    session: parent,
                    member: MemberId::new(),
                    child,
                    handle: "main/lead-1".to_string(),
                    outcome: ReportOutcome::Failed,
                    report: "blocked on tests".to_string(),
                },
                parent,
            ),
            (
                Event::HandoffCommitted {
                    session: child,
                    handle: "main/lead-1".to_string(),
                    generation: 1,
                    doc: "1. Goal - ship".to_string(),
                    degraded: false,
                },
                child,
            ),
            (
                Event::AgentArchived {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child,
                    reason: ArchiveReason::Reported,
                },
                root,
            ),
            (
                Event::AgentArchived {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child,
                    reason: ArchiveReason::Killed,
                },
                root,
            ),
            (
                Event::AgentArchived {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child,
                    reason: ArchiveReason::RootTeardown,
                },
                root,
            ),
            (
                Event::AgentArchived {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child,
                    reason: ArchiveReason::ArchivedByParent,
                },
                root,
            ),
            (
                Event::AgentArchived {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child,
                    reason: ArchiveReason::Shutdown,
                },
                root,
            ),
            (
                Event::AgentRestarted {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child,
                    epoch: ActorEpoch::INITIAL,
                },
                root,
            ),
        ];
        for (event, owner) in cases {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, back, "orchestration event must round-trip: {json}");
            assert_eq!(back.session(), Some(owner), "owning session for {json}");
        }
    }

    /// `parent: None` must not appear on the wire, so a root registration
    /// serializes byte-identically to how it did before scoping. Without
    /// `skip_serializing_if`, every existing log comparison and golden fixture
    /// would shift by one key.
    #[test]
    fn absent_parent_is_omitted_from_the_wire() {
        let session = SessionId::new();
        let rooted = Event::AgentRegistered {
            session,
            agent_session: session,
            handle: "main".to_string(),
            parent: None,
            agent_type: AgentName::new("build"),
            mode: SubagentMode::Transient,
        };
        let json = serde_json::to_string(&rooted).expect("serialize");
        assert!(
            !json.contains("parent"),
            "a None parent must be omitted, got {json}"
        );

        let nested = Event::AgentRegistered {
            session,
            agent_session: SessionId::new(),
            handle: "worker-1".to_string(),
            parent: Some("main/lead-1".to_string()),
            agent_type: AgentName::new("worker"),
            mode: SubagentMode::Resident,
        };
        let json = serde_json::to_string(&nested).expect("serialize");
        assert!(
            json.contains(r#""parent":"main/lead-1""#),
            "a real parent must be carried, got {json}"
        );
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(nested, back);
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
    fn legacy_token_usage_decodes_with_known_flag_default() {
        let json = r#"{"input":5,"output":4,"reasoning":1,"cache_read":2,"cache_write":0}"#;
        let usage: TokenUsage = serde_json::from_str(json).expect("legacy usage decodes");
        assert!(!usage.reasoning_unknown);
        assert_eq!(usage.prompt(), 7);
        assert_eq!(usage.visible_output(), Some(3));
    }
}
