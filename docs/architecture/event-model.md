# Event Model

The event model lives in [`../../crates/hya-proto`](../../crates/hya-proto).
It is shared by the engine, store, provider layer, server, client, and TUI.

## Strong Ids

[`ids.rs`](../../crates/hya-proto/src/ids.rs) defines distinct newtypes for:

| Id | Wire / display |
| --- | --- |
| `SessionId` | New ids: `hysec_[A-Za-z0-9]{20}`; still parses legacy `ses_...` / raw UUID forms |
| `MessageId` | UUIDv7 with `msg_` prefix |
| `PartId` | UUIDv7 with `part_` prefix |
| `ToolCallId` | UUIDv7 with `tc_` prefix |
| `MemberId` | UUIDv7 with `mbr_` prefix |
| `TeamRunId` | UUIDv7 with `team_` prefix |
| `GoalId` | UUIDv7 with `goal_` prefix |
| `LoopRunId` | UUIDv7 with `loop_` prefix |
| `PermissionRequestId` | UUIDv7 with `perm_` prefix |
| `QuestionRequestId` | UUIDv7 with `q_` prefix |
| `ConfigGeneration` | Transparent `u64` (immutable runtime snapshot identity; `INITIAL = 1`) |
| `ActorEpoch` | Transparent `u64` (resident actor incarnation; independent of config generation) |
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
for the native event bus. Compat permission frames use a separate payload shape
(see [Permission SSE payloads](#permission-sse-payloads-server-side)).

Durable Events are append-only and immutable after persistence. A projection may
replace its current derived value while folding a later event, but it never
rewrites, retries, or deletes an earlier envelope. Historical tool errors and
their original typed values therefore remain visible in replay. Live-only
`seq: 0` envelopes are not persisted and are outside durable idempotence; a
fresh replay uses the persisted final state rather than reconstructing live
stream deltas.

### Full `Event` catalog (54 variants)

Reducer effects:

- **fold** — updates `SessionProjection` / `TeamProjection`
- **no-op** — accepted on the wire / log but ignored by `Projection::apply_event`
- **compat / UI only** — same as no-op for the core reducer; consumers may still
  bridge or display them

#### Session lifecycle

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `session_created` | `session: SessionId`, `parent: Option<SessionId>`, `agent: AgentName`, `model: ModelRef`, `workdir: String` | Fold: sets session id, parent, agent, model, workdir. `parent` is the link `session_lineage` walks toward the team root. |
| `session_moved` | `session`, `workdir: String` | Fold: workdir |
| `session_titled` | `session`, `title: String` | Fold: title |
| `session_metadata_set` | `session`, `metadata: Value` | Fold: replaces metadata |
| `session_permission_set` | `session`, `permission: Vec<Value>` | Fold: **replaces** the whole permission list (does not merge) |
| `session_archived` | `session`, `archived: Number` | Fold: archived stamp |
| `session_share_set` | `session`, `url: String` | Fold: share url |
| `session_share_cleared` | `session` | Fold: share → `None` |
| `agent_switched` | `session`, `message: Option<MessageId>`, `agent: AgentName` | Fold: session agent only (`message` is **not** stored on the session row). Engine emit always sets `message: Some(MessageId::new())` — a **fresh** id that is **not** a pointer into existing `SessionProjection.messages`. Compat uses that id as the identity of a **synthetic** switch pseudo-message in the message list (`session_context_messages`), not as a transcript anchor. |
| `model_switched` | `session`, `message: Option<MessageId>`, `model: ModelRef` | Fold: session model only. Same `message` semantics as `agent_switched` (fresh synthetic id on emit). |
| `session_status` | `session`, `status: Value` | **no-op** — free-form status ping; bridged to compat `session.status` |
| `command_executed` | `session`, `command: String`, `arguments: String`, `message: MessageId` | **no-op** — records that a `/slash` command produced that user message; bridged to compat `command.executed` |

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
| `message_started` | `session`, `message: MessageId`, `role: Role` | Fold: creates `MessageProjection` if missing |
| `turn_binding_recorded` | `session`, `message`, `generation: ConfigGeneration` | Fold: `config_generation` on that message. Engine emits it immediately after `MessageStarted{Assistant}` so the immutable runtime snapshot identity is durable before any provider call. |
| `user_prompt_context_recorded` | `session`, `message`, `files: Vec<Value>`, `agents: Vec<Value>` | Fold: prompt `@file` / `@agent` attachment metadata. Engine **emits nothing** when both vectors are empty. |
| `message_finished` | `session`, `message`, `role`, `finish: FinishReason`, `tokens: Option<TokenUsage>` | Fold: finish + tokens. Engine force-emits this with `error` or `cancelled` on turn failure / sidecar loss so clients never wait forever after `message_started`. |
| `message_deleted` | `session`, `message` | Fold: retain-by-id removal of the whole message |
| `part_deleted` | `session`, `message`, `part: PartId` | Fold: removes that part from the message |

#### Step markers (provider rounds)

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `step_started` | `session`, `message`, `step: u32` | **no-op** (UI / compat) |
| `step_finished` | `session`, `message`, `step: u32`, `finish: FinishReason` | **no-op**. `finish` defaults to `stop` when replaying older logs that lacked the field (`#[serde(default = "default_step_finish_reason")]`). |

One pair marks one provider stream round inside an assistant message.

#### Text streaming

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `text_start` | `session`, `message`, `part: PartId` | Fold: empty `PartProjection::Text` |
| `text_delta` | `session`, `message`, `part`, `delta: String` | Fold: append delta |
| `text_replace` | `session`, `message`, `part`, `text: String` | Fold: wholesale overwrite. Live path used when the `text_complete` plugin hook rewrites text. |
| `text_end` | `session`, `message`, `part` | **no-op** — text is already accumulated |

Field name for streaming chunks is **`delta`**, not `text`.

#### Reasoning streaming

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `reasoning_start` | `session`, `message`, `part` | Fold: empty `PartProjection::Reasoning` |
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
TUI to render a completed coding block after live delivery or Session replay.
The hya-owned presentation layer reads projected SDK state only; it does not
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
| `member_spawned` | `session` (parent), `member: MemberId`, `child: Option<SessionId>`, `subagent_type: AgentName`, `description: String`, `depth: u32` | Fold: upsert `MemberProjection`, status → `spawning` |
| `member_status_changed` | `session`, `member`, `status: MemberRunStatus` | Fold: status |
| `member_finished` | `session`, `member`, `status: MemberRunStatus`, `summary: String`, `child: Option<SessionId>` | Fold: status + bounded summary; optional child update if `Some` |

`MemberRunStatus` wire values: `spawning`, `running`, `done`, `failed`,
`cancelled`.

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
| `context_compacted` | `session`, summary `message`, `strategy`, `from_message`, `to_message`, `folded_count`, `input_tokens_est`, `threshold` | **no-op** for projection; durable checkpoint marker. The system message carries summary output and the range points to the folded log entries. |
| `session_forked` | `session`, `source`, optional `before_message` | **no-op** for projection; records a fork edge separate from subagent `SessionCreated.parent`. Copied messages receive fresh ids. |
| `context_evicted` | `session`, `evicted_parts`, `tokens_before`, `tokens_after`, `threshold` | **no-op**; request-local tool-output reduction. The event log retains full outputs. |

`ContextCompacted` is durable replay evidence and a baseline/checkpoint marker,
not a deletion of the source transcript. A projection reader uses the summary
message and the pointer range; an offline reader can reconstruct the exact
folded input from the event log. `ContextEvicted` records only what was omitted
from one request.

#### Errors and forward compatibility

| Wire `type` | Payload fields | Reducer |
| --- | --- | --- |
| `error` | `session: Option<SessionId>`, `code: String`, `message: String` | **no-op** for projection. `session` optional so a global error is expressible; `Event::session()` returns `None` when absent. Bridged to compat `session.error`. |
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
- `session_forked`
- `context_evicted`
- `error`
- `unknown`

## Permission SSE payloads (server-side)

These are **not** `Event` enum variants. `hya-server` parks `AskRequest`s in a
per-server pending map and publishes separate JSON frames to permission
subscribers:

```json
{
  "id": "evt_hya_perm_<request_id>",
  "type": "permission.asked",
  "properties": { /* sessionID, permission, patterns, tool correlation, always/remember, ... */ }
}
```

```json
{
  "id": "evt_hya_perm_reply_<request_id>",
  "type": "permission.replied",
  "properties": {
    "sessionID": "...",
    "requestID": "...",
    "reply": "once" | "always" | "reject"
  }
}
```

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

### `TokenUsage`

Five counters (all `u64`, default 0):

| Field | Serde notes |
| --- | --- |
| `input` | Also accepts alias `prompt` on decode |
| `output` | Also accepts alias `completion` on decode |
| `reasoning` | |
| `cache_read` | |
| `cache_write` | |

**Two aggregation rules (do not conflate them):**

1. `TokenUsage::merge` takes the **max** per field — providers often re-report
   cumulative totals within a stream.
2. The turn loop **sums** counters across provider rounds when building the
   final `MessageFinished.tokens` (`saturating_add` in `add_tokens`).

This is **not** the `token_ledger` row shape (session/role/iteration/run-id
columns in storage). Ledger accounting and envelope `TokenUsage` are different
models.

### `CostBreakdown`

```text
CostBreakdown { input_usd: f64, output_usd: f64 }
```

Defined as the per-message USD cost pair in `hya-proto`. The store schema has a
`message.cost_json` column; no current workspace writer was found that
constructs `CostBreakdown` into that column, so integrators should not assume
live cost population from projection alone.

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
| `title` | `session_titled` |
| `metadata` | `session_metadata_set` |
| `permission` | `session_permission_set` (replace) |
| `archived` | `session_archived` |
| `share` | `session_share_set` / `session_share_cleared` |
| `messages` | message lifecycle + part events |
| `members` | member lifecycle (parent log) |

### `MessageProjection` fields

| Field | Source |
| --- | --- |
| `id`, `role` | `message_started` |
| `config_generation` | `turn_binding_recorded` |
| `finish`, `tokens` | `message_finished` |
| `files`, `agents` | `user_prompt_context_recorded` |
| `parts` | text / reasoning / tool events |

### `PartProjection` — no media arm

`PartProjection` is tagged on `kind` with **exactly three** variants:

| `kind` | Fields |
| --- | --- |
| `text` | `id`, `text` |
| `reasoning` | `id`, `text`, `provider_data?` |
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
- `message_started` creates a message row in memory.
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
- `context_compacted`, `session_forked`, and `context_evicted` remain durable
  observability records rather than projection state transitions; the
  `context_compacted` system message and pointer range provide the replay
  checkpoint.

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
   `GET /sessions/:id/events` (store replay) will never return.
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
a separate projection table for the current read path. `read_projection` replays
the session and folds through the shared reducer.

## Version and restart boundary

The 0.36.9 coding-tool schemas and runtime are selected when a backend starts.
An already-running 0.36.8 backend must restart before future calls use the
canonical hashline Read/Edit/Grep, closed Write, or canonical Bash contracts.
Replaying a Session does not rewrite its history: captured 0.36.8 Read or Task
errors remain the original durable `tool_error` Events. Hashline snapshots and
duplicate/recovery guards are process-local, so restart discards that transient
state while current-file anchor validation and durable Event replay remain
available.
