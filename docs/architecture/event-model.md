# Event Model

The event model lives in [`../../crates/hya-proto`](../../crates/hya-proto).
It is shared by the engine, store, provider layer, and server, which fold
`hya_proto::Projection`. Rust clients over the `hya.v1` contract do not fold
this projection directly either: `hya-sdk-v1`'s `V1SessionMirror` folds the
curated v1 `StreamFrame`/read shapes into an equivalent client-side view, and
server-side reads (`ListMessages`, `GetSessionTodo`, …) are produced by folding
this same projection before serialization. The legacy TypeScript TUI (now
removed) consumed the deleted Compat SDK/SyncProvider surface; a replacement
frontend built on `hya-sdk-v1` may be built later.

## Strong Ids

[`ids.rs`](../../crates/hya-proto/src/ids.rs) defines distinct newtypes for:

| Id | Wire / display |
| --- | --- |
| `SessionId` | New ids: `hysec_[A-Za-z0-9]{20}`; still parses legacy `ses_...` / raw UUID forms |
| `MessageId` | UUIDv7 with `msg_` prefix |
| `PartId` | UUIDv7 with `part_` prefix |
| `ToolCallId` | UUIDv7 with `tc_` prefix |
| `OperationId` | Display `op_` + UUID-simple; durable tool-call operation identity (UUID v5 from `ToolCallId`) |
| `MemberId` | UUIDv7 with `mbr_` prefix |
| `TeamRunId` | UUIDv7 with `team_` prefix |
| `WorkflowRunId` | UUIDv7 with `wfrun_` prefix; durable Workflow run identity (also UUID v5 from `OperationId`) |
| `GoalId` | UUIDv7 with `goal_` prefix |
| `LoopRunId` | UUIDv7 with `loop_` prefix |
| `PermissionRequestId` | UUIDv7 with `perm_` prefix |
| `QuestionRequestId` | UUIDv7 with `q_` prefix |
| `ConfigGeneration` | Transparent `u64` (immutable runtime snapshot identity; `INITIAL = 1`) |
| `ActorEpoch` | Transparent `u64` (resident actor incarnation; independent of config generation) |
| `OwnerRunId` | Transparent UUID (random v4); per-process runtime ownership and recovery fence |
| `EventSeq` | Transparent `u64` (see [EventSeq semantics](#eventseq-semantics)) |

The strong types keep different ids from being accidentally swapped at compile
time.

### EventSeq semantics

`EventSeq` is the **globally monotonic** `event_log.seq` value:
`INTEGER PRIMARY KEY AUTOINCREMENT` on a single shared `event_log` table
([`0001_init.sql`](../../crates/hya-store/migrations/0001_init.sql)). It is
**not** per-session. Gaps between consecutive envelopes of one session are
normal; clients must treat session sequences as strictly increasing but not
contiguous.

**`seq: 0` is reserved** for live-only, never-persisted publishes from
`SessionEngine::publish_live` (high-frequency text deltas during a provider
round). Those envelopes are applied by the projection reducer without advancing
`last_seq` (see [Projection::apply](#projectionapply)).

Assistant text of a provider round is live-only while it streams:
`text_start`, each `text_delta`, `text_end` (and a `text_replace` when the
`text_complete` hook rewrote the part) are published with `seq: 0`. When the
round's stream ends, the engine appends one durable `text_start` +
`text_replace` (the final text) + `text_end` per text part, with the same
message and part ids. The v1 streams deliver both (live frames bypass the
`sinceSeq` filter); `ListEvents` and replay only see the durable ones.
Reasoning, tool-input, and user-message deltas are durable. The durable text
part is appended at the end of its round, so its position among the round's
other parts can differ from the live arrival order; the projection order is
authoritative.

## Events and Envelopes

[`event.rs`](../../crates/hya-proto/src/event.rs) defines `Event`, the
canonical runtime stream:

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event { ... }
```

Wire JSON is tagged on `"type"` with snake_case variant names (for example
`"message_started"`, `"tool_call_requested"`).

An `Envelope` wraps an event with:

| Field | Type | Meaning |
| --- | --- | --- |
| `seq` | `EventSeq` | Store rowid, or `0` for live-only |
| `ts_millis` | `i64` | Unix epoch milliseconds |
| `event` | `Event` | Payload |

The envelope is the unit stored in SQLite replay results and streamed over SSE
for the event bus. Pending permission/question requests are **not** envelopes:
the server parks them in a per-process pending plane and surfaces them through
the v1 Interactions service (see
[Pending permission plane (server-side)](#pending-permission-plane-server-side)).

Durable Events are append-only and immutable after persistence. A projection may
replace its current derived value while folding a later event, but it never
rewrites, retries, or deletes an earlier envelope. Historical tool errors and
their original typed values therefore remain visible in replay. Live-only
`seq: 0` envelopes are not persisted and are outside durable idempotence; a
fresh replay uses the persisted final state rather than reconstructing live
stream deltas.

### Full `Event` catalog (70 variants)

Reducer effects:

- **fold** — updates `SessionProjection` / `TeamProjection`
- **no-op** — accepted on the wire / log but ignored by `Projection::apply_event`
- **compat / UI only** — same as no-op for the core reducer; consumers may still
  bridge or display them

#### Session lifecycle

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `session_created` | `session: SessionId`, `parent: Option<SessionId>`, `agent: AgentName`, `model: ModelRef`, `workdir: String` | Fold: sets session id, parent, agent, model, workdir. `parent` is the link `session_lineage` walks toward the team root. |
| `session_agent_model_override_set` | `session`, `agent: AgentName`, `model: Option<ModelRef>` | Fold: insert or remove one Agent entry in `agent_model_overrides`. `None` clears that Agent; unrelated entries remain. |
| `session_permission_mode_set` | `session`, `mode: String` | Fold: `permission_mode` (last write wins). Emitted only on the lineage root; subagent sessions inherit the root's mode. `mode` is `manual`, `yolo`, or `<bundle-id>/<mode-id>`; see [Session permission modes](../configuration.md#session-permission-modes). Older binaries fold it as `unknown` (no-op). The v1 stream maps it to `sessionUpdated.permissionMode`. |
| `session_moved` | `session`, `workdir: String` | Fold: workdir |
| `session_titled` | `session`, `title: String` | Fold: title |
| `session_metadata_set` | `session`, `metadata: Value` | Fold: replaces metadata |
| `session_permission_set` | `session`, `permission: Vec<Value>` | Fold: **replaces** the whole permission list (does not merge) |
| `session_archived` | `session`, `archived: Number` | Fold: archived stamp |
| `session_share_set` | `session`, `url: String` | Fold: share url |
| `session_share_cleared` | `session` | Fold: share → `None` |
| `agent_switched` | `session`, `message: Option<MessageId>`, `agent: AgentName` | Fold: session agent only (`message` is **not** stored on the session row). Engine emit always sets `message: Some(MessageId::new())` — a **fresh** id that is **not** a pointer into existing `SessionProjection.messages`. (The deleted Compat surface used that id as the identity of a **synthetic** switch pseudo-message in its message list, not as a transcript anchor.) |
| `model_switched` | `session`, `message: Option<MessageId>`, `model: ModelRef` | Fold: session model only. Same `message` semantics as `agent_switched` (fresh synthetic id on emit). |
| `session_status` | `session`, `status: Value` | **no-op** — free-form status ping; the deleted Compat surface bridged it to `session.status` |
| `command_executed` | `session`, `command: String`, `arguments: String`, `message: MessageId` | **no-op** — records that a `/slash` command produced that user message; the deleted Compat surface bridged it to `command.executed` |

#### Workflow lifecycle and routing

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `workflow_selected` | `session`, `workflow: WorkflowIdentity` | Fold: replace the selected Workflow identity; transcript messages are preserved. |
| `workflow_run_started` | `session`, `run`, `workflow`, `request_hash`, `owner`, `stages: Vec<WorkflowStagePlan>` | Fold: create the durable run and declaration-ordered plan. The plan carries display/provenance metadata, not directives or outputs. |
| `workflow_stage_started` | `session`, `run`, `stage` | Fold: mark one compiled Stage active. |
| `workflow_stage_member_linked` | `session`, `run`, `stage`, `member`, `role`, `iteration` | Fold: link the canonical worker or verifier Member to a Stage activation. |
| `workflow_stage_route_outcome` | `session`, `run`, `stage`, `member`, `role`, `iteration`, `step`, `candidate_index`, `model`, `reasoning`, `failure_class` | Fold: append one bounded candidate selection/failure observation for a provider stream group. It contains no prompt, response, credential, or provider text. |
| `workflow_stage_finished` | `session`, `run`, `stage`, `status` | Fold: terminalize one Stage. |
| `workflow_run_finished` | `session`, `run`, `status`, optional `error` | Fold: terminalize the run with bounded error detail when present. |

Workflow events are appended to the owning root Session log. Stage/member
transcripts remain in child Sessions; the route outcome is replay metadata, not
model output. Explicit Stage or loop-verifier assignments use a suffix-free
preferred model plus ordered fallback candidates and per-candidate reasoning.


#### Message lifecycle

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `message_started` | `session`, `message: MessageId`, `role: Role`, optional `agent: AgentName`, optional `model: ModelRef` | Fold: creates `MessageProjection` if missing, with `agent` / `model` copied onto it. The engine sets both on assistant turns: the session's agent and the model the turn's first round requests (explicit request, session override, user-file model, authored policy, then session model). The model that actually served each round is recorded separately by `usage_recorded`. User, system, and shell messages omit both, as do logs written before 0.41.0 (projection reducer version 4). A fork copies the source message's agent and served model. |
| `turn_binding_recorded` | `session`, `message`, `generation: ConfigGeneration` | Fold: `config_generation` on that message. Engine emits it immediately after `MessageStarted{Assistant}` so the immutable runtime snapshot identity is durable before any provider call. |
| `user_prompt_context_recorded` | `session`, `message`, `files: Vec<Value>`, `agents: Vec<Value>` | Fold: prompt `@file` / `@agent` attachment metadata, and prompt images: one `files` entry `{type: "image_attachment", part, name, mime, size, blob, path?}` per image, where `blob` is the sha256 of the bytes in the session's `file_blob` table (the bytes never ride on the event). An image prompt appends it in the same transaction as the user message, between `text_end` and `message_finished`. Engine **emits nothing** when both vectors are empty. v1: `partsAdded` with one `AttachmentPart` per image (no other entries map). |
| `message_finished` | `session`, `message`, `role`, `finish: FinishReason`, `tokens: Option<TokenUsage>`, `cause: Option<FinishCause>` | Fold: finish + cause + tokens. `tokens` is the legacy sum of the message's rounds and is only set on a normal finish. Engine force-emits this with `error` or `cancelled` (and `tokens: None`) on turn failure, cancel, drain, sidecar loss, or crash recovery so clients never wait forever after `message_started` (see [End-event invariant](#end-event-invariant)); `cause` says why (omitted when the model ended the message, and absent in logs written before 0.41.0). Billed rounds of such a message are still counted through `usage_recorded`. For a message with no `usage_recorded` record (a legacy log) a non-zero `tokens` is folded once into `SessionProjection.usage` under model `unattributed`, except in forked sessions, whose copied messages carry the source's sums. |
| `message_deleted` | `session`, `message` | Fold: retain-by-id removal of the whole message |
| `part_deleted` | `session`, `message`, `part: PartId` | Fold: removes that part from the message |

#### Step markers (provider rounds)

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `step_started` | `session`, `message`, `step: u32` | **no-op** (UI / compat) |
| `step_finished` | `session`, `message`, `step: u32`, `finish: FinishReason` | **no-op**. `finish` defaults to `stop` when replaying older logs that lacked the field (`#[serde(default = "default_step_finish_reason")]`). |

One pair marks one provider stream round inside an assistant message. The
round's billed usage and serving model are recorded separately by
`usage_recorded` (below), emitted between the two markers once the stream ends.

#### Token accounting

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `usage_recorded` | `session`, optional `message: MessageId`, optional `step: u32`, `model: ModelRef`, `purpose: UsagePurpose`, `tokens: TokenUsage` | Fold: adds `tokens` to `SessionProjection.usage` under `model` and `purpose`; for a `turn` round also sums it into that message's `MessageProjection.usage`. Session totals are never decremented. |

One record per provider call that reported usage:

- `purpose: turn` — every streaming round of an assistant turn, with `message`
  and `step`. `model` is the model that served the round: the request model
  after `chat.params`, then after the cross-model fallback chain or the
  `model.fallback` hook, or the selected Workflow route candidate. A round
  that delivered usage and then failed, or a round of a message that later
  ended `cancelled`/`error`, is still recorded.
- `purpose: title` — automatic title generation.
- `purpose: compaction` — summarizer calls: the compaction ladder's
  `summarize`/`handoff` rungs, `/compact` (`summarize_session`), and the
  terminal handoff document.

Side calls (`title`, `compaction`) omit `message` and `step` and never create
transcript messages. `UsagePurpose` is `turn` \| `title` \| `compaction`; an
unknown value from a newer binary decodes as `other`. An older binary folds the
whole variant as `unknown`. The v1 curated stream does not map it (the IDL's
`TokensRecorded` payload stays unconstructed); read the fold from the
projection. Not attributed today: provider-native compaction
(`/responses/compact` reports no usage to the engine), goal evaluators, and
loop verifiers. Usage of a stream that is dropped before its decoder reports
usage (mid-stream cancel or transport failure) is not known to the engine and
is not recorded.

#### Text streaming

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `text_start` | `session`, `message`, `part: PartId` | Fold: empty `PartProjection::Text` |
| `text_delta` | `session`, `message`, `part`, `delta: String` | Fold: append delta. Assistant deltas are live-only (`seq: 0`); user-message text and logs written before the live split are durable. |
| `text_replace` | `session`, `message`, `part`, `text: String` | Fold: wholesale overwrite. The durable record of every assistant text part (final text), and live when the `text_complete` plugin hook rewrites text. v1: `partReplaced`. |
| `text_end` | `session`, `message`, `part` | **no-op** — text is already accumulated |

Field name for streaming chunks is **`delta`**, not `text`.

#### Reasoning streaming

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `reasoning_start` | `session`, `message`, `part`, `reason: Option<String>` | Fold: empty `PartProjection::Reasoning` (copies `reason`) |
| `reasoning_delta` | `session`, `message`, `part`, `delta: String` | Fold: append |
| `reasoning_end` | `session`, `message`, `part`, `provider_data: Option<Value>` | Fold: stores `provider_data` (opaque provider state such as encrypted thinking blocks — must be round-tripped back to the provider verbatim) |
| `reasoning_replace` | `session`, `message`, `part`, `text: String` | Fold: wholesale overwrite |

Unlike text, reasoning events are **not** re-batched as a durable triple; they
take the normal durable `emit_for_actor` path inside `collect_stream_round`.

#### Tool lifecycle

| Wire `type` | Payload fields | ToolPartState | Reducer |
| --- | --- | --- | --- |
| `tool_input_start` | `session`, `message`, `part`, `call: ToolCallId`, `name: ToolName` | → `Pending { input: null }` | Fold: push tool part |
| `tool_input_delta` | `session`, `message`, `part`, `call`, `name`, `delta: String` | (unchanged) | **no-op** (compat bridge may forward raw argument JSON) |
| `tool_call_requested` | `session`, `message`, `part`, `call`, `name`, `input: Value` | → `Running { input }` | Fold: upsert running tool. Turn loop collects these into the round's `tool_calls` list. |
| `tool_result` | `session`, `message`, `part`, `call`, `output: Value`, `time_ms: u64` | → `Completed { input, output, time_ms }` | Fold |
| `tool_error` | `session`, `message`, `part`, `call`, `message_text: String`, `value: Option<Value>` | → `Error { input, message, value }` | Fold. Engine commonly sets `value` to `{ "error": { "type": "...", "message": "..." } }`. |
| `tool_part_updated` | `session`, `message`, `part`, `state: ToolPartState` | → given state | Fold: direct overwrite (fork/copy and out-of-band progress) |

##### Coding-tool result payloads

`tool_call_requested` stores the canonical model input; `tool_result` stores the
successful `{title, output, metadata}` value; and `tool_error` stores the typed
`{error:{type,message}}` value when available. These are ordinary durable tool
events, not a coding-tool-specific event family or a second result store. The
shape-aware cap keeps bounded Read/Grep/Bash output and host presentation
metadata structured, while Edit may retain a separately bounded diff. Every
metadata collection has independent byte/row limits and explicit truncation;
metadata cannot bypass the result cap. Provider replay consumes an object's
string `output` field and falls back to serialized JSON only when that field is
absent.

The projected `ToolPartState::Completed` value is therefore sufficient for a
client to render a completed coding block after live delivery or Session replay.
The presentation layer reads projected SDK state only; it does not
read or fold raw Events. A malformed or compacted result remains a typed
completed/error value for the projection and uses the presentation fallback,
not arbitrary input-key rendering. `env` values and ANSI terminal control data
are not presentation metadata.

When a tool-level cancellation reaches the terminal tool-event boundary, it is a
durable `tool_error` with wire type `cancelled`; an actor-level turn cancellation
may instead terminate the turn and finish the surrounding message/step with
`cancelled` without emitting a per-call result. A nonzero Bash exit or timeout
remains a completed structured result with status metadata; it is not converted
to a durable `ToolError` unless execution itself is cancelled or fails before a
terminal result is built.

#### Member lifecycle (parent log)

These attach to the **parent** session so the agent tree is observable without
leaking child transcripts. They carry only bounded metadata + a short summary.

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `member_spawned` | `session` (parent), `member: MemberId`, `child: Option<SessionId>`, `subagent_type: AgentName`, `description: String`, `depth: u32`, `directive: String`, `tool_call: Option<ToolCallId>` | Fold: upsert `MemberProjection`, status → `spawning`; copies `directive` when non-empty and `tool_call` when `Some` |
| `member_status_changed` | `session`, `member`, `status: MemberRunStatus` | Fold: status |
| `member_finished` | `session`, `member`, `status: MemberRunStatus`, `summary: String`, `child: Option<SessionId>` | Fold: status + bounded summary; optional child update if `Some` |

`MemberRunStatus` wire values: `spawning`, `running`, `done`, `failed`,
`cancelled`.

A resident member's row (ADR-0015) moves through: `member_spawned`
(`spawning`; a `task` spawn records the call's `description` and
`tool_call`) → `member_status_changed { running }` when a turn starts and the
row is not already running (once per episode: the first turn after the spawn
or after a revival, never once per wake; idle between wakes stays `running`) →
terminal: `subagent_reported` (`done`/`failed`, report as summary — also the
engine-synthesized failure report of a turn error), `member_finished
{ cancelled }` from `archive`/drain/stop, or `member_finished { failed }` from
a budget kill or other failure finalization. The terminal writes check the
folded row first, so a repeated stop or finalize appends nothing.

v1 exposes these (and `subagent_reported`) as the durable `memberUpdated`
stream event on the parent session and folds `MemberProjection` rows into
`SessionInfo.members` ([protocol guide](../protocol/README.md#subagents)).

**Log placement:** member lifecycle events live on the **parent** log.
`AgentRegistered` / `AgentActivityChanged` / `MailSent` / channel events live
on the **team-root** log. Those are different logs whenever the parent is not
the root.

#### Team / roster / mail (team-root log)

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `agent_registered` | `session` (team root), `agent_session: SessionId`, `handle: String` (the agent's **leaf** name), `parent: Option<String>` (default absent = team root), `agent_type: AgentName` (default empty), `mode: SubagentMode` (default `transient`) | Fold: roster entry keyed by **canonical path** `{parent}/{handle}`; a root registration (`agent_session == session`) keys as `main`. See [ADR-0011](../adr/0011-hierarchy-scoped-mailbox.md) |
| `agent_activity_changed` | `session`, `handle`, `status: RosterStatus`, `current_task: Option<String>` | Fold: roster activity; idle/terminal clears in-flight resident work and advances durable cursor |
| `resident_work_started` | `session`, `actor_session`, `handle`, `epoch: ActorEpoch`, `inbox_through: u64` | Fold: marks fenced resident work on roster before tool/child/provider dispatch |
| `mail_sent` | `session`, `from: String` (canonical path), `to: MailEndpoint` (a canonical path, or a unit-qualified channel key `{unit}#{name}`), `kind: MailKind` (default), `body: String` | Fold: direct → recipient inbox; channel → channel log + fan-out to current **eligible** subscribers (skips any member whose roster entry is `mode.is_resident()` **and** status is `Done` or `Failed`; see [ADR-0001](../adr/0001-event-sourced-mailbox-and-channels.md)). Addresses are resolved to canonical form at **send** time, not fold time; see [ADR-0011](../adr/0011-hierarchy-scoped-mailbox.md) |
| `channel_joined` | `session`, `channel: String`, `member: String` | Fold: add subscriber |
| `channel_left` | `session`, `channel`, `member` | Fold: remove subscriber |

`SubagentMode`: `transient` \| `resident`.  
`RosterStatus`: `idle` \| `busy` \| `done` \| `failed`.

#### Context, fork, and reduction observability

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `context_compacted` | `session`, summary `message`, `strategy`, `from_message`, `to_message`, `folded_count`, `input_tokens_est`, `threshold` | **no-op** for projection; durable checkpoint marker. The system message carries summary output and the range points to the folded log entries. A manual compaction (`CompactSession` / `SummarizeSession`) records `strategy: local_summarizer` over the whole window it summarized, with `threshold: 0` (no threshold tripped); v1 maps it to `compactionApplied { manual: true }`. |
| `todos_updated` | `session`, `todos: [TodoItem { id, content, status }]` | Fold: `SessionProjection.todos` = the full list. Appended by the engine right after a todo tool's `tool_result` whose `metadata.todos` differs from the folded list (reads and no-op writes append nothing). Sessions without it (logs before reducer version 5) read the latest todo tool result instead. |
| `session_forked` | `session`, `source`, optional `before_message` | Fold: `SessionProjection.forked_from = source`, `forked_before = before_message` (the source-log user message the copy stopped before; `None` for a head fork, which copies every visible message). Records a fork edge separate from subagent `SessionCreated.parent`. Copied messages receive fresh ids; their copied `tokens` never count toward the fork's `usage`. v1: `SessionInfo.forkedFrom`. `forked_before` folds from projection reducer version 6 (0.41.0). |
| `context_evicted` | `session`, `evicted_parts`, `tokens_before`, `tokens_after`, `threshold` | **no-op**; request-local tool-output reduction. The event log retains full outputs. |

`ContextCompacted` is durable replay evidence and a baseline/checkpoint marker,
not a deletion of the source transcript. A projection reader uses the summary
message and the pointer range; an offline reader can reconstruct the exact
folded input from the event log. `ContextEvicted` records only what was omitted
from one request.

#### File snapshots and revert

See [Runtime — File snapshots and revert](runtime.md#file-snapshots-and-revert)
for what is captured, the limits, and the restore rules. File contents never
ride on events: `FileState` names a per-session blob (`file_blob` table).

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `files_changed` | `session`, `message` (assistant message of the call), optional `call: ToolCallId`, `files: [FileChange { path, before: FileState }]` | Fold: appends `FileChangeRecord { call, path, before }` to that message's `file_changes`. Appended right after the call's `tool_result` / `tool_error` when it changed at least one file. |
| `session_reverted` | `session`, `message` (the reverted user message), `files: [FileRestore { path, restored, saved, error? }]` (omitted when empty) | Fold: `message` and every later message move from `messages` to `revert.hidden` (`SessionProjection.revert = { message, hidden, files }`). While a revert is pending, a new one extends it: the new hidden messages come first, and a file already listed keeps its first `saved`. A `message` not in `messages` is a no-op. v1: `sessionReverted { messageId, files }`. |
| `session_unreverted` | `session`, `files: [FileRestore]` (omitted when empty) | Fold: the hidden messages return to the end of `messages`; `revert = None`. No-op when nothing is pending. v1: `sessionReverted { undone: true, files }`. |

`FileState` is tagged on `kind`: `absent` (no regular file), `stored { hash,
size }` (lowercase hex sha256 of the content, the blob key), or `omitted {
size, reason }` (content not kept: `too_large`, `session_cap`,
`snapshot_budget`, `unreadable`; never restored). In a `FileRestore`,
`restored` is the state the operation wrote, `saved` the state it replaced
(what an unrevert writes back), and `error` why the write failed.

**Commit.** There is no commit event: the fold of the next `message_started`
for a new message (any role) drops a pending revert first
(`revert = None`), so the hidden messages are gone for good and an unrevert
after it is a no-op. Logs written before 0.41.0 contain none of these events
and fold exactly as before (`revert` and `file_changes` stay empty and are
omitted from the serialized projection); an older binary folds them as
`unknown`. Projection reducer version 6.

#### Errors and forward compatibility

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `error` | `session: Option<SessionId>`, `code: String`, `message: String`, `failed_message: Option<MessageId>` | Fold only with `failed_message`: sets `MessageProjection.error = {code, message}` on that message; otherwise **no-op**. The engine appends it when a turn fails (`code`: `provider_error`, `tool_error`, `store_error`, …; `message`: the error's display text, at most 2000 bytes) immediately before the message's `message_finished { finish: error }`. `session` optional so a global error is expressible; `Event::session()` returns `None` when absent. v1: `errorReported`, `MessageInfo.error`, `TurnInfo.errorCode`/`errorMessage`. `failed_message` is absent in logs written before 0.41.0 (projection reducer version 3). |
| `unknown` | (unit; original payload dropped on typed decode) | **no-op**. `#[serde(other)]` catch-all so older binaries can deserialize newer tags without failing. Lossless forwarding must keep raw JSON. |

### Events the reducer does not fold

These variants are accepted on the wire and may appear in the log or live bus,
but `Projection::apply_event` ignores them:

- `session_status`
- `command_executed`
- `step_started`
- `step_finished`
- `text_end`
- `tool_input_delta`
- `context_compacted`
- `context_evicted`
- `error` without `failed_message`
- `unknown`

## Pending permission plane (server-side)

These are **not** `Event` enum variants. `hya-server` parks `AskRequest`s in a
per-server pending map
([`pending/permission.rs`](../../crates/hya-server/src/pending/permission.rs)).
Over the wire, clients see this plane only through the v1 Interactions service:
`GET /v1/interactions` lists pending permission/question requests as
`Interaction` summaries, and `POST /v1/interactions/{id}/respond` answers one
(idempotent — `applied` is `false` on replay). Saved `always` rules are listed
and deleted through the same service.

Internally the plane still builds legacy Compat-shaped `permission.asked` /
`permission.replied` JSON frames and keeps a broadcast bridge for a future
interaction event stream; nothing on the current HTTP surface subscribes to
them.

**Fan-out when answering one request**
(`take_related_for_reply` in
[`permission.rs`](../../crates/hya-server/src/pending/permission.rs)):

| Reply | Related pending in the same session |
| --- | --- |
| `once` | **None** — only the answered request is resolved |
| `always` | Cascades to every other pending request sharing the same `RememberScope` (and action rules for legacy scopes) |
| `reject` + exact remember scope | Cascades like always for that exact scope |
| `reject` + legacy action scope | Cascades to **every** other pending permission request in that session (no action or remember-scope filter; `take_related_for_reply` sets related scope to `None`) |

An `always` reply may also persist a saved permission; `once` does not cascade.

**Switching a tree to `yolo`** (`UpdateSession.permission_mode`) resolves
every pending request whose session's lineage root is that tree's root as
`once`, after the `session_permission_mode_set` event is durable, and
publishes the usual replied notification for each. An ask that reaches the
plane after the switch (a call that read the mode just before it changed) is
allowed on arrival: the arrival check and the sweep take the same pending
lock, so no ask of a `yolo` tree stays pending.

## Messages and Parts

[`message.rs`](../../crates/hya-proto/src/message.rs) defines the model-facing
message shape (tagged on `role` / `type`):

| Type | Meaning |
| --- | --- |
| `Message::User` | User content as parts. |
| `Message::Assistant` | Assistant content, model/agent metadata, finish reason, optional usage. |
| `Message::System` | System content string. |
| `Part::Text` | Text content. |
| `Part::Reasoning` | Reasoning text + optional `provider_data`. |
| `Part::Media` | Model-facing media: MIME type, data, optional filename. |
| `Part::Tool` | Tool call state (`call_id`, `name`, `ToolPartState`). |

### `FinishReason`

Snake_case wire values on both `MessageFinished` and `StepFinished`:

| Wire | Meaning |
| --- | --- |
| `stop` | Normal completion |
| `tool_calls` | Model requested tools (round continues) |
| `length` | Output length limit |
| `cancelled` | Cancel token or sidecar loss |
| `error` | Hard failure |

Terminal state of **both** a finished message and a finished provider step.
`StepFinished.finish` defaults to `stop` when absent from older logs.

### `FinishCause`

Optional context on `MessageFinished` (never on `StepFinished`) when the
harness, not the model, ended an assistant message. `FinishReason` stays the
classification (`cancelled` / `error`); there is no extra finish reason.
Additive serde: omitted when `None`, and an unknown value from a newer build
decodes as `other`.

| Wire | `finish` | Written when |
| --- | --- | --- |
| `user_cancel` | `cancelled` | A user stopped the turn: `/v1` turn cancel (`SessionEngine::cancel_turn`), SIGINT on `exec`/`run`/`-p`/`loop` |
| `shutdown` | `cancelled` | Graceful process stop: end of a one-shot run, SIGTERM, `serve` shutdown (the drain) |
| `leader_failed` | `cancelled` | A member turn drained because its lead's turn failed and the one-shot run ended |
| `interrupted` | `cancelled` | The process died with the turn open; closed by startup crash recovery |
| `provider_error` | `error` | The model provider failed the turn (`CoreError::Provider`) |
| `archived` | `cancelled` | The member's parent archived it with the `archive` tool while it was mid-turn |
| `other` | any | A cause this build does not know (forward compatibility) |

Other cancels (team budget kill, sidecar loss, a parent turn's cancel reaching
a child) and non-provider runtime errors carry no cause. The `hya.v1` wire
mirrors the enum as `FinishCause` on `MessageFinished.cause` and
`MessageInfo.finish_cause` (`FINISH_CAUSE_UNSPECIFIED` when absent).

### End-event invariant

1. Every assistant message ends with **exactly one** `message_finished`.
2. Every non-terminal tool part (`pending` / `running`) reaches a terminal
   state (`tool_error` with `value.code` `CANCELLED`, `TURN_FAILED`, or
   `INTERRUPTED` when the harness closes it).
3. Every member reaches a terminal status (`member_finished { cancelled }` for
   member rows; roster `failed` for resident members stopped by a drain).
   A resident whose `task` call already returned is not part of its parent's
   turn: closing that turn (cancel, crash recovery) leaves its row open, and
   the resident's own report, archive, or finalization closes it.
   The lead (the root session's `main`) is never made terminal or archived.

The engine keeps it on every path: a cancelled or failed turn closes its own
message (open tool parts first, then the finish, checked against the folded
log so nothing is closed twice); a graceful stop drains every in-flight turn in
every session; a crash is repaired by the next runtime owner before any turn
runs. See [Runtime — Turn termination guarantees](runtime.md#turn-termination-guarantees).

### `TokenUsage`

Five counters (all `u64`, default 0) plus one flag. Every provider decoder
normalizes to this invariant (see
[providers.md](providers.md#token-usage-normalization)):

| Field | Meaning | Serde notes |
| --- | --- | --- |
| `input` | Uncached prompt tokens; **excludes** `cache_read` and `cache_write` | Also accepts alias `prompt` on decode |
| `output` | All generated tokens, **including** thinking | Also accepts alias `completion` on decode |
| `reasoning` | Thinking tokens, a subset of `output`; 0 when unknown | |
| `cache_read` | Prompt tokens read from cache | |
| `cache_write` | Prompt tokens written to cache (cache creation) | |
| `reasoning_unknown` | The provider did not report the thinking share of `output` (Anthropic); the split is unknown, never estimated | `bool`, omitted when `false` |

The whole prompt is `input + cache_read + cache_write` (`TokenUsage::prompt`);
visible output is `output - reasoning` when the split is known
(`TokenUsage::visible_output`, `None` otherwise).

Legacy logs (written before this invariant) carry provider-native values —
OpenAI `input` included cached tokens, Google `output` excluded thoughts,
Anthropic reported `reasoning: 0` — and no `reasoning_unknown` field. Readers
treat the thinking split of legacy usage as unknown; the projection fold does
so for every legacy `MessageFinished.tokens` sum.

**Two aggregation rules (do not conflate them):**

1. `TokenUsage::merge` takes the **max** per field — providers often re-report
   cumulative totals within a stream.
2. The turn loop **sums** counters across provider rounds when building the
   final `MessageFinished.tokens` (`TokenUsage::add`).

In both, `reasoning_unknown` is sticky: once any sample or round is unknown,
the result is.

### Session usage fold

`SessionProjection.usage: SessionUsage` (in
[`usage.rs`](../../crates/hya-proto/src/usage.rs)) is the replayable per-model
account of billed tokens for **one** session log:

```text
SessionUsage {
  by_model:   BTreeMap<ModelRef, UsageTotals>,     // serving model; legacy under "unattributed"
  by_purpose: BTreeMap<UsagePurpose, UsageTotals>, // turn | title | compaction (| other)
}
UsageTotals {
  input, cache_read, cache_write, output,  // sums under the TokenUsage invariant
  reasoning,                 // thinking of calls that reported it (subset of output)
  reasoning_unknown_output,  // output of calls whose thinking split is unknown
  rounds,                    // usage_recorded records folded
  legacy_messages,           // legacy MessageFinished sums folded
}
```

- Sources: every `usage_recorded`; plus, for a message without any
  `usage_recorded`, its non-zero `MessageFinished.tokens` once, as model
  `unattributed` (`UNATTRIBUTED_MODEL`), purpose `turn`, thinking unknown.
  Messages with records never fall back, so nothing is counted twice. Forked
  sessions skip the fallback.
- Billed stays billed: `message_deleted`, revert, and compaction never
  decrement it.
- `UsageTotals::output_split() -> OutputSplit { thinking, visible, unknown }`
  keeps partial knowledge (`thinking + visible + unknown == output`);
  `OutputSplit::thinking_exact()` / `visible_exact()` return `Some` only when
  `unknown == 0`. `UsageTotals::prompt()` is `input + cache_read + cache_write`.
- Child (subagent) sessions keep their own logs; aggregate a tree by folding
  each session and calling `SessionUsage::merge`. `SessionUsage::total()` sums
  every model.

This is **not** the `token_ledger` row shape (session/role/iteration/run-id
columns in storage). Ledger accounting and envelope `TokenUsage` are different
models.

The store schema keeps a `message.cost_json` column, but no workspace writer
populates it and `hya-proto` no longer defines a cost type for it; integrators
should not assume live cost population from projection alone.

### `ToolPartState`

Tagged on `phase`:

```text
pending  -> running -> completed
                   \-> error
```

| Phase | Fields |
| --- | --- |
| `pending` | `input: Value` |
| `running` | `input: Value` |
| `completed` | `input`, `output`, `time_ms` |
| `error` | `input`, `message`, `value: Option<Value>` |

## Projection

[`projection.rs`](../../crates/hya-proto/src/projection.rs) folds ordered
envelopes into a `Projection`:

```text
Projection {
  session: SessionProjection,
  team: TeamProjection,   // mail/channels/roster; empty when unused
  last_seq: u64,
}
```

### `SessionProjection` fields

| Field | Source events |
| --- | --- |
| `id`, `parent`, `agent`, `model`, `workdir` | `session_created` (+ switch/move) |
| `agent_model_overrides` | `session_agent_model_override_set` (`None` model removes that Agent) |
| `permission_mode` | `session_permission_mode_set` on the root (omitted when `None`; `None` means the process default: `yolo` under `--yolo`/`model: danger`, else `manual`) |
| `title` | `session_titled` |
| `metadata` | `session_metadata_set` |
| `permission` | `session_permission_set` (replace) |
| `archived` | `session_archived` |
| `share` | `session_share_set` / `session_share_cleared` |
| `messages` | message lifecycle + part events |
| `members` | member lifecycle (parent log) |
| `workflow` | workflow lifecycle events |
| `context_status` | latest `context_status` |
| `forked_from` | `session_forked` (omitted when `None`) |
| `forked_before` | `session_forked.before_message` (omitted when `None`) |
| `revert` | pending `session_reverted` (`{ message, hidden: [MessageProjection], files: [FileRestore] }`): the hidden messages live here, not in `messages`, until `session_unreverted` or the next `message_started` (omitted when `None`) |
| `usage` | `usage_recorded` + legacy `message_finished.tokens` fallback (omitted when empty) |
| `todos` | latest `todos_updated` (omitted when `None`) |

### `MessageProjection` fields

| Field | Source |
| --- | --- |
| `id`, `role` | `message_started` |
| `agent`, `model` | `message_started` (requested model; omitted when `None`). `MessageProjection::served_model()` prefers `usage.model` (the model that served the latest round, after fallback or routing) and falls back to `model`. |
| `time_created`, `time_updated` | Envelope `ts_millis` (Unix ms): the message's `message_started`, and the newest event folded onto the message (lifecycle, part events including live `seq == 0` deltas, `usage_recorded`, `error` with `failed_message`, `message_finished`). Step markers do not move it. Omitted when `None`. |
| `config_generation` | `turn_binding_recorded` |
| `finish`, `cause`, `tokens` | `message_finished` (`cause` omitted when `None`) |
| `usage` | `usage_recorded` with this `message`: `MessageUsage { model /* latest round */, tokens /* sum */, rounds, last_round /* latest round alone */ }` (omitted when `None`) |
| `files`, `agents` | `user_prompt_context_recorded` |
| `parts` | text / reasoning / tool events |
| `file_changes` | `files_changed` for this message: `[{ call?, path, before: FileState }]` in call order (omitted when empty) |
| `error` | `error` with `failed_message` = this message: `MessageError { code, message }` (omitted when `None`) |

### `PartProjection` — no media arm

`PartProjection` is tagged on `kind` with **exactly three** variants:

| `kind` | Fields |
| --- | --- |
| `text` | `id`, `text` |
| `reasoning` | `id`, `text`, `reason?`, `provider_data?` |
| `tool` | `id`, `call`, `name`, `state` |

`Part::Media` exists on the **model-facing** `Message` / `Part` value types
(for provider request building) but has **no** `PartProjection` counterpart.
Media attachments are **not** reconstructed by `read_projection` / replay fold.
Anyone who needs media after a projection read must go to the raw event log (or
another store of attachments).

### High-level fold behavior

- `session_created` sets session metadata.
- Session metadata / title / archive / share / move / switch events update
  session state.
- `message_started` creates a message row in memory, carrying the turn's
  agent and requested model and the envelope time as `time_created`.
- `turn_binding_recorded` stores the assistant message's lightweight
  `ConfigGeneration`; registry contents remain outside the event log.
- `user_prompt_context_recorded` preserves prompt attachment metadata.
- Text and reasoning starts create parts; deltas append; replacements overwrite.
- `reasoning_end` stores `provider_data`.
- Tool call requests upsert running tool parts; results / errors /
  `tool_part_updated` finalize or replace tool state.
- Delete events remove messages or parts from the projected view.
- `message_finished` records finish reason and tokens.
- Member events fold the parent's `members` list.
- Team-root mail/roster events fold `Projection.team`.
- `resident_work_started` records epoch and inbox boundary before a resident
  turn may dispatch; a later idle/terminal activity clears it and advances the
  roster's durable resident cursor.
- Workflow lifecycle events fold the selected identity, run plan, Stage/member
  links, bounded route outcomes, and terminal statuses into the Workflow
  projection. `WorkflowStageRouteOutcome` is one observation per provider
  stream group and never carries transcript content.
- `files_changed` appends file-change records to its message.
- `session_reverted` moves the reverted user message and everything after it
  into `revert.hidden`; `session_unreverted` moves them back; a new
  `message_started` commits (drops) a pending revert.
- `session_forked` records the fork source and cut; `context_compacted` and
  `context_evicted` remain durable observability records rather than
  projection state transitions; the `context_compacted` system message and
  pointer range provide the replay checkpoint.

### `Projection::apply`

```rust
pub fn apply(&mut self, env: &Envelope) {
    if env.seq.0 == 0 {
        self.apply_event(&env.event);
        return; // does NOT advance last_seq
    }
    if env.seq.0 <= self.last_seq {
        return; // durable idempotence
    }
    self.apply_event(&env.event);
    self.last_seq = env.seq.0;
}
```

Interpretation:

1. **`seq == 0`** — live-only publish (`publish_live`). Applied unconditionally
   and **does not** advance `last_seq`. Deliberately outside the durable
   idempotence guarantee; must not be replayed from the store (store never
   assigns seq 0).
2. **`seq <= last_seq`** — no-op. Makes SSE reconnect and duplicate durable
   delivery safe.
3. **else** — apply and advance `last_seq`.

Callers may ignore **older durable** envelopes after reconnect. They must not
treat seq-0 redelivery as a no-op.

## Live-only vs durable streaming

During a provider round, `collect_stream_round`
([`stream_round.rs`](../../crates/hya-core/src/engine/stream_round.rs)):

1. **Text** events (`text_start` / `text_delta` / `text_end`, plus live
   `text_replace` from `text_complete`) are published with **`publish_live`
   (seq 0)** and are **not** persisted as the raw stream.
2. After the stream ends, each completed text part is re-emitted **durably** as:

   ```text
   text_start → text_replace (final content) → text_end
   ```

3. **`tool_call_requested`** events are collected into the round's
   `tool_calls` list **and** still take the durable emit path when not text.
4. Provider-emitted **`message_finished`** is **swallowed**: its `finish` and
   `tokens` become the `StreamRound` result; the turn loop emits the real
   assistant `message_finished` later (after tools, or when the turn ends).
5. Reasoning and other non-text events use durable `emit_for_actor` immediately.

Consequences:

1. An SSE subscriber can see many `text_delta` frames that
   `GET /v1/sessions/{session}/events` (store replay) will never return.
2. A replay of the log yields final text in one `text_replace` instead of the
   live delta stream.
3. **Projection state is identical either way**, which is what makes the two
   paths interchangeable for read models that use the reducer.

## Provider Boundary

Provider decoders produce canonical events, not provider-specific objects. For
example, OpenAI-compatible, Anthropic, and Google tool-call streams all become
`Event::ToolCallRequested`, even though their wire formats differ.

The engine is responsible for executing tool calls and appending
`Event::ToolResult` or `Event::ToolError`.

## Store Boundary

The store serializes `Event` JSON into `event_log.payload`. It does not maintain
a separate read model: `read_projection` always equals folding the session's
log through the shared reducer. The fold is cached — in-process and as durable
`projection_snapshot` rows keyed by session and last folded `seq`, tagged with
`PROJECTION_REDUCER_VERSION` — so a read applies only the events after the
cached fold. The cache is derivable from the event log alone and is discarded
whenever its anchor event or reducer version no longer matches; see
[storage.md](storage.md#projection-cache). Changing the reducer's fold result
for existing events requires bumping `PROJECTION_REDUCER_VERSION`.

Write-through side tables are maintained in the append transaction for
queries that must not replay every log. `open_assistant_message
(session_id, message_id)` (migration `0010`) holds assistant messages that
started but have not finished or been deleted; it is read only by startup
crash recovery (`SessionStore::recover_interrupted_turns`), so recovery costs
O(sessions a dead process left mid-turn). The migration backfills it from
existing logs in one pass over the `message_*` rows.

## Version and restart boundary

The 0.36.9 coding-tool schemas and runtime are selected when a backend starts.
An already-running 0.36.8 backend must restart before future calls use the
canonical hashline Read/Edit/Grep, closed Write, or canonical Bash contracts.
Replaying a Session does not rewrite its history: captured 0.36.8 Read or Task
errors remain the original durable `tool_error` Events. Hashline snapshots and
duplicate/recovery guards are process-local, so restart discards that transient
state while current-file anchor validation and durable Event replay remain
available.
