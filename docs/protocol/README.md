# hya Protocol Guide (v1)

This guide explains how to integrate any client — GUI, WebUI, CLI, service, or
a future TUI — with the hya backend over the v1 API. The same contract is served
over two transports with identical functionality:

- **HTTP/JSON + SSE + WebSocket** (documented here; see
  [`openapi.json`](./openapi.json) and the generated
  [`api-reference.md`](./api-reference.md) for the full surface).
- **gRPC** (`hya.v1` package, server reflection enabled; the proto files
  under `proto/hya/v1` are the source of truth).

## Base URL and scoping

All HTTP routes live under `/v1`. The backend serves one process-default
directory; requests that accept a scope take a `directory` field (query
parameter for GETs, body field otherwise). The `x-hya-directory` request
header overrides the field on any request. gRPC clients pass the same
values via the `hya-directory` metadata key.

## Versioning

`/v1` is additive-only within a major version: new fields and rpcs appear
without notice, unknown fields must be ignored by clients. Breaking changes
ship as `/v2` side by side.

## Serialization rules (protojson)

Bodies follow canonical protojson:

- Fields are `lowerCamelCase`; unset fields are omitted (do not treat
  absence as an error).
- Enums serialize as their full value names: `"TURN_STATE_RUNNING"`,
  `"AUTH_STATUS_CREDENTIALED"`.
- 64-bit integers serialize as strings: `"nextSeq": "42"`.
- `bytes` fields are standard base64 strings.
- Timestamps are RFC 3339 strings.

## Errors

Every failure renders:

```json
{ "error": { "code": "session_not_found", "message": "session not found: hysec_..." } }
```

Stable codes and their HTTP status / gRPC code:

| Code | HTTP | gRPC | Meaning |
| --- | --- | --- | --- |
| `invalid_argument` | 400 | `InvalidArgument` | Malformed request. |
| `not_found` | 404 | `NotFound` | Resource does not exist. |
| `session_not_found` | 404 | `NotFound` | Unknown or deleted session. |
| `permission_denied` | 403 | `PermissionDenied` | Caller not authorized. |
| `session_busy` | 409 | `FailedPrecondition` | Another run owns the session. |
| `conflict` | 409 | `FailedPrecondition` | State conflict (stale revision, patch rejection). |
| `unavailable` | 503 | `Unavailable` | Required capability not configured (e.g. no summarizer, OAuth not wired). |
| `internal` | 500 | `Internal` | Unhandled failure. |
| `bundle_api_not_found` | 404 | `NotFound` | No published bundle endpoint matches (unknown bundle, bundle without endpoints, or no template of the scope matches the path under any method). |
| `bundle_api_method_not_allowed` | 405 | `Unimplemented` | The path matches bundle endpoints of the scope, but not under this method; HTTP lists the allowed ones in `Allow`. |
| `bundle_api_bad_request` | 400 | `InvalidArgument` | Malformed bundle API request: body over 512 KiB or not JSON, bad percent escape, unparsable query, unknown method (gRPC). |
| `bundle_api_failed` | 502 | `Unavailable` | The bundle process failed, timed out, or answered malformed data while serving an endpoint. |

## Pagination

Paginated list rpcs take `page: {cursor, limit}` and answer
`page: {nextCursor, hasMore}`. Cursors are opaque; pass `nextCursor` back
verbatim. Over HTTP GET, send them as `page.cursor` and `page.limit` query
parameters. Events use the monotonic `sinceSeq` watermark instead.

## The event-driven model

1. `POST /v1/sessions` creates a session (`agent`, `model`, `workdir`).
2. `GET /v1/bootstrap` fetches config + catalogs + pending interactions in
   one round trip at startup.
3. `POST /v1/sessions/{id}/turns` admits work — body is a `oneof` of
   `prompt`, `command`, or `shell` — and returns a `RUNNING` turn handle
   immediately. For prompt and command turns the turn id is the id of the
   admitted **user** message; the assistant message arrives on the stream
   as `messageStarted` with `ROLE_ASSISTANT`. Slash commands that route to
   workflow features execute synchronously and return a `FINISHED` turn
   with an empty id. There is no server-side prompt queue: while a turn
   runs, another `CreateTurn` fails with `409 session_busy`; queue
   follow-up prompts client-side and submit them after the assistant
   `messageFinished`.
4. Subscribe to `GET /v1/sessions/{id}/events/stream` (SSE) or
   `GET /v1/events/stream` (global) **before** admitting the turn: the
   streams deliver from the moment of subscription and do not replay
   history (`sinceSeq` only skips durable events at or below it). Every
   projection change arrives as a `StreamFrame` JSON object; the turn ends
   with the assistant `messageFinished`. On lag the server sends a
   `resync` frame — see [Live and durable frames](#live-and-durable-frames).
5. Synchronous clients may poll `GET /v1/sessions/{id}/turns/{turn}` or use
   `POST .../turns/{turn}/wait` with `timeoutMs`. `POST .../cancel`
   requests a cooperative abort.

SSE frames are `data:` lines containing one `StreamFrame`:

```json
{ "event": { "seq": "12", "session": "hysec_...", "timeRecorded": "...", "messageFinished": { "message": "msg_...", "finish": "FINISH_REASON_STOP" } } }
```

```json
{ "resync": { "lastSeq": "40" } }
```

When the harness rather than the model ended an assistant message,
`messageFinished.cause` (and `MessageInfo.finishCause`) says why:
`FINISH_CAUSE_USER_CANCEL` (turn cancel), `FINISH_CAUSE_SHUTDOWN` (graceful
server/run stop), `FINISH_CAUSE_LEADER_FAILED`, `FINISH_CAUSE_INTERRUPTED`
(closed by crash recovery on the next start), `FINISH_CAUSE_PROVIDER_ERROR`,
`FINISH_CAUSE_ARCHIVED` (the member's parent archived it mid-turn), or
`FINISH_CAUSE_OTHER`; it is omitted (unspecified) otherwise. Every
`messageStarted` for an assistant message is followed by exactly one
`messageFinished` — also across a server stop or crash.

Assistant messages carry their own attribution, independent of the
session's current binding, so a transcript read after a `/model` or agent
switch still labels older messages correctly:

- `MessageInfo.agent` is the agent the message's turn ran as.
- `MessageInfo.model` (`provider/model`) is the model that served the
  message's latest round (after `chat.params`, fallback, or routing), or the
  model the turn requested when no round has reported usage yet.
- `messageStarted.agent` / `.model` carry the same agent and the requested
  model live, so a streaming header is right before the transcript re-read.
- `MessageInfo.timeCreated` is when the message started; `timeUpdated` is
  the newest change to it (parts, live deltas, usage, error, finish). For a
  finished message `timeUpdated - timeCreated` is its elapsed time.

User, system, and shell messages, and messages recorded before 0.41.0,
leave `agent` empty; clients fall back to the session binding there.

```json
{ "event": { "seq": "31", "session": "hysec_...", "messageFinished": { "message": "msg_...", "finish": "FINISH_REASON_CANCELLED", "cause": "FINISH_CAUSE_SHUTDOWN" } } }
```

## Session titles

A root session without a title gets one automatically after its first
prompt (or command) turn: the server asks the fixed `title` agent in the
background and records it as `SessionTitled`, which streams as durable
`sessionUpdated { title }` and shows in `SessionInfo.title` — usually shortly
after the turn starts, never blocking it. Subagent (child) sessions, sessions
created or renamed with a title (`CreateSession.title`, `UpdateSession`), and
later turns are never titled; a manual rename always wins. The title call is
billed to the session (`tokensRecorded` with an empty `message`).

## Usage and context occupancy

`TokenUsage` follows one invariant on every provider
([providers.md](../architecture/providers.md#token-usage-normalization)):
`input` excludes the cache, so the whole prompt is
`input + cacheRead + cacheWrite`; `output` includes thinking; `reasoning` is
the thinking share of `output`, and `reasoningUnknown` says the provider did
not report it (for a sum: some summed call did not). All counts are `uint64`,
so protojson sends them as strings, and zero fields are omitted.

- `ModelSummary.contextLimit` / `.outputLimit` (`GET /v1/models`): the
  model's context window (configured `limit.context`, else the route's
  advertised default) and output ceiling (`limit.output`); `0`/absent when
  unknown.
- `MessageInfo.usage`: billed usage of an assistant message, the sum of its
  provider rounds (the finish total for messages recorded before per-round
  usage). `MessageInfo.roundUsage`: its latest round alone, served by
  `MessageInfo.model`.
- `SessionInfo.usage`: everything billed for the session (turn rounds plus
  title/summarizer side calls). It never decreases.
- `tokensRecorded { message, model, usage }` (durable): one per provider call
  as it is billed; `message` is empty for side calls.

**Context occupancy** is the prompt the latest round sent:
`roundUsage.input + roundUsage.cacheRead + roundUsage.cacheWrite` of the
newest assistant message that has `roundUsage`, against the
`contextLimit` of its `model`. Live, take the newest `tokensRecorded` with a
non-empty `message` instead. For **session totals** read `SessionInfo.usage`
(re-read it on `tokensRecorded`); summing messages would miss side calls.

```json
{ "event": { "seq": "18", "session": "hysec_...", "tokensRecorded": { "message": "msg_...", "model": "openai/gpt-5", "usage": { "input": "30", "output": "10", "cacheRead": "1150" } } } }
```

## Todos

`GET /v1/sessions/{id}/todo` (`GetSessionTodo`) returns the session's todo
list: `items[] { id, content, status }` with `TODO_STATUS_PENDING`,
`_IN_PROGRESS`, `_BLOCKED`, or `_COMPLETED`. Whenever a todo tool changes
it, the session stream carries the whole new list as durable
`todoUpdated { items }` (same rows); a read that changes nothing sends none.
Sessions whose list was last edited by an older hya have no `todoUpdated`
record; the read still returns their list (taken from the todo tool results)
until the next edit records one.

```json
{ "event": { "seq": "22", "session": "hysec_...", "todoUpdated": { "items": [ { "id": "1", "content": "write tests", "status": "TODO_STATUS_IN_PROGRESS" } ] } } }
```

## Compaction

When part of the context is folded behind a summary, the session stream
carries durable `compactionApplied { untilSeq, strategy, message,
foldedCount, manual }`: `message` is the system message holding the summary
(the transcript divider), `foldedCount` the number of messages folded behind
it, and `manual` is true for a client-requested `CompactSession` (or
`SummarizeSession`, which compacts the same way) and false when the context
crossed its threshold mid-turn. `strategy` is `Native`, `LocalSummarizer`
(also every manual compaction), `SnapCompact`, or `Handoff`. The record is
in `ListEvents` too, so a transcript read can place the divider.

```json
{ "event": { "seq": "40", "session": "hysec_...", "compactionApplied": { "untilSeq": "40", "strategy": "LocalSummarizer", "message": "msg_...", "foldedCount": 12, "manual": true } } }
```

## Live and durable frames

Stream events come in two kinds:

- **Durable** events carry their log `seq` (a string, `"12"`). They are in
  the event log, `ListEvents` replays them, and the projection
  (`ListMessages`, `GetMessage`) folds them.
- **Live-only** events carry no `seq` (it is `0`, which protojson omits).
  They are never persisted and `ListEvents` never returns them: the
  assistant text of an in-flight provider round, and the pending
  interaction frames (`permissionRequested`, `questionRequested`,
  `interactionResolved`; `GET /v1/interactions` is their listing).

While a provider round streams, each assistant text part arrives live as
`partStarted` (`kind: "text"`), one `partAppended` per delta, and
`partCompleted`. When the round's stream ends, the durable log records the
same part once, with the **same** message and part ids: `partStarted`,
`partReplaced` (`text` = the final full text), `partCompleted`. Reasoning
deltas and user-message text are durable `partStarted` / `partAppended` /
`partCompleted` events; tool-call arguments are durable `partStarted` /
`partAppended` followed by `toolStateChanged` (see [Tool calls](#tool-calls)).
A `text_complete`
plugin may rewrite a finished part: the rewrite arrives as a live
`partReplaced` and is what the durable `partReplaced` records.

A client that renders streaming text folds frames **by id**:

1. `partStarted` for a part id it already has is not a new part (the live
   and the durable start of one part).
2. `partAppended` appends `textDelta` to that part.
3. `partReplaced` sets the part's whole text; it supersedes the live deltas.
4. The durable text part is recorded at the end of its round, so its
   position among the round's other parts in the projection can differ from
   the live arrival order. After the assistant `messageFinished`, re-read the
   message (`GET /v1/sessions/{id}/messages`) and render the projection; it
   is authoritative.

```json
{ "event": { "session": "hysec_...", "timeRecorded": "...", "partAppended": { "message": "msg_...", "part": "part_...", "textDelta": "Hel" } } }
{ "event": { "seq": "41", "session": "hysec_...", "timeRecorded": "...", "partReplaced": { "message": "msg_...", "part": "part_...", "text": "Hello!" } } }
```

**Lag and reconnect.** The engine bus buffers 8192 envelopes (configurable)
per subscriber. A subscriber that falls further behind receives one
`resync` frame per lag and loses the frames in the gap, live deltas
included; nothing is dropped from the durable log. `resync.lastSeq` is the
`sinceSeq` the stream was opened with. After a `resync` or a reconnect,
re-read the projection (`ListMessages`) — or replay
`ListEvents(sinceSeq = <last durable seq you applied>)` — and continue
with the stream. A part whose live deltas were lost is completed by its
durable `partReplaced`. The v1 SDK's `V1SessionMirror` implements these
rules.

## Tool calls

A tool call is one `ToolCallPart` (`PartInfo.toolCall`) for its whole life;
there is no separate result part. On a transcript read it carries:

| Field | Set when |
| --- | --- |
| `callId`, `tool` | always |
| `state` | always: `TOOL_EXECUTION_STATE_PENDING` (arguments streaming), `_RUNNING`, `_OK`, `_ERROR` |
| `inputJson` | the call's arguments as JSON text, in every state once known (empty while still streaming) |
| `outputJson`, `durationMs` | `OK`: the stored output as JSON text (the same size-capped value the model saw) and the wall time; protojson omits a zero `durationMs` |
| `errorCode`, `errorMessage` | `ERROR`: the structured `error.type` (`unknown` when absent; e.g. `input`, `permission`) and the error text |

The stream carries the same data as durable events, in order:

1. `partStarted` with `kind: "tool_call"`, `tool`, and `callId`.
2. `partAppended` per argument fragment; `textDelta` is raw JSON text to
   append to `inputJson`.
3. `toolStateChanged` `RUNNING` with `callId`, `tool`, and the parsed
   `inputJson`, which replaces the appended fragments.
4. `toolStateChanged` `OK` with `outputJson` and `durationMs`, or `ERROR`
   with `errorCode` and `errorMessage`.

Fields a frame does not carry are empty; fold them into the part by
`part` id without clearing what you already have. A direct part
overwrite (fork copy, out-of-band progress) arrives as `toolStateChanged`
with the full state and an empty `callId`.

```json
{ "event": { "seq": "14", "session": "hysec_...", "partStarted": { "message": "msg_...", "part": "part_...", "kind": "tool_call", "tool": "bash", "callId": "call_..." } } }
{ "event": { "seq": "15", "session": "hysec_...", "partAppended": { "message": "msg_...", "part": "part_...", "textDelta": "{\"command\":\"ls\"}" } } }
{ "event": { "seq": "16", "session": "hysec_...", "toolStateChanged": { "message": "msg_...", "part": "part_...", "callId": "call_...", "state": "TOOL_EXECUTION_STATE_RUNNING", "inputJson": "{\"command\":\"ls\"}", "tool": "bash" } } }
{ "event": { "seq": "19", "session": "hysec_...", "toolStateChanged": { "message": "msg_...", "part": "part_...", "callId": "call_...", "state": "TOOL_EXECUTION_STATE_OK", "outputJson": "{...}", "durationMs": "42" } } }
```

## Subagents

A subagent spawn (the `task` tool, or a resident member) is recorded on the
**parent** session and streams there as durable `memberUpdated` events
(`MemberInfo`): `member`, `child` (child session id), `agent` (subagent
type), `description`, `status` (`MEMBER_STATUS_SPAWNING`, `_RUNNING`,
`_DONE`, `_FAILED`, `_CANCELLED`), `summary` (bounded, on finish),
`callId`, and `depth`. The spawn frame carries every field; a status
change carries `member` and `status`; a finish adds `summary` and `child`.
Fold by `member`. `GET /v1/sessions/{id}` (`SessionInfo.members`) returns
the folded rows, so a client that reconnects mid-task still has them, and
`GET /v1/sessions?parent={id}` lists the child sessions.

Every subagent is a resident with one task at a time (ADR-0015), so the
statuses mean:

| Status | Meaning |
| --- | --- |
| `MEMBER_STATUS_SPAWNING` | Registered; its first turn has not started. |
| `MEMBER_STATUS_RUNNING` | Its task is open: working on a turn **or idle** waiting for mail. Sent once when a turn starts, not on every wake; use the child's `SessionInfo.busy` to tell working from idle. |
| `MEMBER_STATUS_DONE` | It reported success; `summary` is the report. |
| `MEMBER_STATUS_FAILED` | It reported failure, a turn failed (the engine files the failure report), it was killed (team budget), or its root session was deleted; `summary` is the reason. |
| `MEMBER_STATUS_CANCELLED` | It was archived by its parent, stopped, or archived by a shutdown drain. |

Mail from its parent revives a finished member: its row goes back to
`MEMBER_STATUS_RUNNING` when the new episode's turn starts. For a `task`
spawn, `description` is the call's `description` and `callId` its call id;
members started without a tool call (Workflow Stages) have an empty
`callId` and a description cut from their directive. The spawn frame is
written before the `task` result, and the first `_RUNNING` may land before or
after it. Cancelling the parent's turn after `task` returned does not cancel
the member.

Link a tool card to its child:

- live: `memberUpdated.callId` equals the spawning `ToolCallPart.callId`,
  and `memberUpdated.child` is the child session;
- after the call: the `task` tool's `outputJson` is
  `{title, metadata: {sessionId, parentSessionId, subagent_type, status}, output}`;
  `metadata.sessionId` is the child session id.

```json
{ "event": { "seq": "21", "session": "hysec_parent", "memberUpdated": { "member": "mem_...", "child": "hysec_child", "agent": "general", "description": "survey the repo", "status": "MEMBER_STATUS_SPAWNING", "callId": "call_...", "depth": 1 } } }
{ "event": { "seq": "25", "session": "hysec_parent", "memberUpdated": { "member": "mem_...", "status": "MEMBER_STATUS_RUNNING" } } }
{ "event": { "seq": "40", "session": "hysec_parent", "memberUpdated": { "member": "mem_...", "child": "hysec_child", "status": "MEMBER_STATUS_DONE", "summary": "found 3 crates" } } }
```

## Errors of a failed turn

When a turn fails (for example a non-retryable provider error), the engine
appends a durable `errorReported` event naming the assistant message just
before that message's `messageFinished` (`finish: FINISH_REASON_ERROR`,
`cause: FINISH_CAUSE_PROVIDER_ERROR` for provider failures):

```json
{ "event": { "seq": "52", "session": "hysec_...", "errorReported": { "message": "msg_...", "code": "provider_error", "errorMessage": "http status 400: ..." } } }
```

The same `{code, message}` is on the message (`MessageInfo.error`) and on
the turn (`TurnInfo.errorCode` / `errorMessage` once `state` is
`TURN_STATE_FAILED`). Codes: `provider_error`, `tool_error`,
`store_error`, `bundle_error`, `runtime_refresh_error`,
`agent_definition_missing`, `invalid`, `turn_already_active`. The message
text is the engine's error display text, at most 2000 bytes (a provider
HTTP error carries a bounded excerpt of the response body). Turns that
failed before 0.41.0 have no recorded error.

## Interactions (permissions and questions)

Pending permission and question requests arrive as `permissionRequested` /
`questionRequested` events (carrying an `Interaction` summary) and are
listed by `GET /v1/interactions` (every type unless `type` is given;
`type=INTERACTION_TYPE_PERMISSION` or `INTERACTION_TYPE_QUESTION` filters).
Answer with
`POST /v1/interactions/{id}/respond` — body is a `oneof` of
`{permission: {allowed, persist}}` or `{question: {answer}}` /
`{question: {rejected: true}}`. The response's `applied` is `false` when
the request was already resolved (idempotent replay).

A permission `Interaction` has `title` `"<action> <resource>"` and a
`payload` object a prompt can render:

| Key | Value |
| --- | --- |
| `action` | permission action (`bash`, `edit`, `read`, `tool`, ...) |
| `resource` | the pattern being decided (the command, the path, the tool name) |
| `always` | patterns an "always" answer (`persist: true`) saves |
| `messageId`, `callId` | the assistant message and tool call that asked (when correlated) |
| `tool` | the tool name of that call |
| `input` | that call's arguments object as recorded on its tool part (`command` for bash; the path and old/new text or patch for edit tools). Numbers arrive as doubles (`Struct`). |

`callId` matches the `ToolCallPart.callId` of the waiting tool card. The
payload exposes only what the transcript's tool part already holds.

A question `Interaction` has `title` = the first question, `detail` = its
header, `options` = its option labels, and `payload`
`{questions: [{question, header, options: [{label, description}], multiple?, custom?}]}`
with every question of the request. The listing returns exactly the
`Interaction` the `permissionRequested` / `questionRequested` frame carried,
so a client that only sees an ask through `GET /v1/interactions` (or gRPC
`ListInteractions`) can render the same prompt.

### Subagent asks on a parent's stream

Interaction frames are live-only (`seq = 0`, so protojson omits `seq`) and
name the session that asked in both `event.session` and
`interaction.session`. The global stream
(`GET /v1/events/stream`, `StreamGlobalEvents`) carries every session's. A
session stream carries only its own session's unless you opt in with
`includeDescendants=true` (query parameter on
`GET /v1/sessions/{id}/events/stream`; `include_descendants: true` on gRPC
`StreamSessionEventsRequest`): it then also delivers the
`permissionRequested`, `questionRequested`, and `interactionResolved` frames
of every session below it (subagents at any depth), with `event.session` set
to the asking descendant — answer them with the same
`POST /v1/interactions/{id}/respond`. Durable events stay per session either
way. Frames sent before you subscribe are not replayed: list
`GET /v1/interactions` after subscribing to catch asks already pending.

```json
{ "event": { "session": "hysec_child", "questionRequested": { "request": "q_...", "interaction": { "id": "q_...", "session": "hysec_child", "type": "INTERACTION_TYPE_QUESTION", "title": "Which branch?", "detail": "Branch", "options": ["main", "dev"], "payload": { "questions": [ { "question": "Which branch?", "header": "Branch", "options": [ { "label": "main", "description": "the default branch" }, { "label": "dev", "description": "" } ], "multiple": true } ] } } } } }
{ "event": { "session": "hysec_child", "interactionResolved": { "request": "q_..." } } }
```

## Permission modes

A session tree's permission mode decides whether asks reach the user at all.
`GET /v1/permission-modes` (`Catalog.ListPermissionModes`) lists the
selectable modes as `{modes: [{id, title, description, source}]}`: the
built-in `manual` and `yolo` (`source: "builtin"`), then each installed
bundle's modes as `<bundle-id>/<mode-id>` (`source`: the bundle id). Set one
with `PATCH /v1/sessions/{session}` and `{"permissionMode": "yolo"}`
(`Session.UpdateSession`); an unknown or unavailable mode fails with
`invalid_argument`. The mode is recorded on the root session and shared by
its subagent sessions: `SessionInfo.permissionMode` reports the effective
mode on every session of the tree, and the root's event stream carries a
`sessionUpdated` event with `permissionMode`. Switching to `yolo` resolves the
tree's pending permission interactions as allowed once (each emits the usual
`interactionResolved` event). Semantics: [Configuration — Session permission
modes](../configuration.md#session-permission-modes).

## Providers and keys

The provider routes back the TUI Provider View (`/key`). Every write applies
**live**: the server rebuilds that provider's route and catalog rows, swaps
them into the running engine, and emits `catalog.updated` on the event
streams — no restart. The effective model list of a provider is its cached
remote models merged per model id with its `config.yaml` `models:` entries;
see [Configuration — Model cache and config
overrides](../configuration.md#model-cache-and-config-overrides).

| Call | HTTP | Body / query | Answer |
| --- | --- | --- | --- |
| `Catalog.ListProviders` | `GET /v1/providers` | — | `{providers: [ProviderSummary], page}` |
| `Catalog.GetProvider` | `GET /v1/providers/{providerId}` | — | `ProviderInfo` |
| `Catalog.UpsertProvider` | `PUT /v1/providers/{providerId}` | `{kind, baseUrl, apiKey?}` | `ProviderUpdate` (fetches) |
| `Catalog.RefreshProvider` | `POST /v1/providers/{providerId}/refresh` | `{}` or empty | `ProviderUpdate` (fetches) |
| `Catalog.SetProviderModel` | `PUT /v1/providers/{providerId}/models` | `{modelId, displayName?, contextLimit?, outputLimit?, reasoning?}` | `ProviderUpdate` |
| `Catalog.RemoveProviderModel` | `DELETE /v1/providers/{providerId}/models?modelId=…` | query `modelId` | `ProviderUpdate` |
| `Catalog.TestProviderModel` | `POST /v1/providers/{providerId}/test` | `{modelId}` | `TestProviderModelResponse` |
| `Auth.ListProviderAuth` | `GET /v1/auth` | — | `{providerIds: [...]}` |
| `Auth.SetProviderAuth` | `PUT /v1/auth/{providerId}` | `{apiKey}` | `{status, provider?, discovery?}` |
| `Auth.RemoveProviderAuth` | `DELETE /v1/auth/{providerId}` | — | `{provider?}` |

Model ids may contain `/` and `:`, so they never travel in the path: the
model id is `modelId` in the body (`PUT …/models`, `POST …/test`) or the
`modelId` query parameter (`DELETE …/models`, percent-encoded).

**Shapes** (protojson; unset fields, `false`, and `0` are omitted):

```jsonc
// ProviderSummary
{ "id": "gw", "name": "gw",
  "auth": "AUTH_STATUS_CREDENTIALED",   // CREDENTIALED when a key exists (saved or config),
                                        // UNAUTHENTICATED without one, AUTH_REJECTED /
                                        // AUTH_REQUIRED after the model list refused the key
  "result": "models",                   // models | empty | unavailable | invalid
  "kind": "openai",                     // config kind; empty for the offline `hya` row
  "baseUrl": "https://gw.example/v1",
  "keySource": "saved",                 // saved | oauth | config | none (never the secret)
  "modelCount": 3 }

// ProviderInfo
{ "summary": ProviderSummary, "models": [ModelSummary],
  "supportsApiKey": true, "supportsOauth": false }

// ModelSummary (also GET /v1/models)
{ "id": "gw/alpha", "providerId": "gw", "modelId": "alpha",
  "displayName": "Alpha", "reasoning": true, "auth": "AUTH_STATUS_CREDENTIALED",
  "contextLimit": "64000", "outputLimit": "4096",   // uint64 → strings
  "source": "override" }                            // remote | config | override | offline

// ProviderUpdate
{ "provider": ProviderInfo,
  "discovery": {                 // only when the call fetched the remote list
    "ok": true,                  // fetched and parsed (possibly empty)
    "result": "models",          // models | empty | auth_required | auth_rejected |
                                 // unavailable | invalid | unsupported
    "errorMessage": "…",         // when ok is false (bounded, non-secret)
    "modelCount": 2 } }

// TestProviderModelResponse
{ "ok": true, "text": "Hi", "finishReason": "length",   // stop | length | tool_calls | cancelled | error
  "errorCode": "http_401",       // when ok is false: http_<status> | transport | timeout |
                                 // unknown_model | incompatible | decode | auth_expired | provider_error
  "errorMessage": "…", "latencyMs": 412 }
```

**Semantics.**

- `UpsertProvider` validates the id (1–64 of `A-Z a-z 0-9 - _`; `hya` is
  reserved), the kind (`openai`, `openai-response`, `anthropic`, `google`;
  the config aliases `openai-compatible`, `openai-completion`, `openai-codex`,
  `grok-build` are accepted), and the base URL (`http(s)://host…`, no
  userinfo). It writes `providers.<id>.kind` / `base_url` into
  `config.yaml` (a new provider gets `models: []`; every other key is
  kept), saves a non-empty `apiKey` to `auth/<id>.yaml`, then fetches the
  model list. A failed fetch does **not** fail the call: `discovery.ok` is
  false and `discovery.errorMessage` says why; the provider is saved.
- `RefreshProvider` re-reads the provider's config entry and credential and
  fetches the model list into the model cache. `404 not_found` when the id
  is not in `config.yaml`. A 401/403 clears the cached rows; a transport or
  decode failure keeps them.
- `SetProviderModel` patches one model entry in the provider's `models:`
  (adding a bare `- <modelId>` entry when there is none). An absent field
  keeps the entry's current value; `displayName: ""` removes `name`;
  `contextLimit: 0` / `outputLimit: 0` remove that `limit.*` key;
  `reasoning` writes a boolean `reasoning` (`true` keeps a detailed
  `reasoning:` mapping). `reasoning` cannot be cleared through this call:
  delete the entry (`RemoveProviderModel`) or edit `config.yaml`. Other keys
  are kept. An `outputLimit` above the entry's `contextLimit` after the
  patch is `invalid_argument`.
  `RemoveProviderModel` deletes the entry (`404` when there is none); a
  remote model stays listed from the cache with `source: "remote"`.
- `SetProviderAuth` writes the key atomically with mode `0600` as
  `type: api` and rebuilds the provider live; when the cache has no rows for
  the provider it also fetches the model list (reported in `discovery`).
  `RemoveProviderAuth` deletes the file and rebuilds (an inline config
  `api_key` applies again). `provider` is omitted for an id that is not in
  `config.yaml`.
- `TestProviderModel` sends one user message `hi` with no system prompt, no
  tools, and no reasoning effort, with max output tokens `1` (`16` on
  Responses routes — `openai-response`, `openai-codex`, `grok-build` —
  whose upstream rejects smaller values), bounded by 60 seconds. `ok` is
  true when the reply stream completed without an error; a `length` finish
  is a normal reply. A model not in the catalog is `404 not_found`; a
  provider failure is `200` with `ok` absent (false) and `errorCode`. Nothing
  is written to any session.
- Without an application provider control (a bare `hya_server::router`
  embedder) the write routes and `GET /v1/auth` answer `503 unavailable`.

## Bundle API endpoints

Installed bundles with an `extensions.process` may register their own HTTP
endpoints (manifest `apis:`; see
[AgentBundle authoring](../agent-bundle-authoring.md#api-endpoints-apis) and
[bundle runtime](../bundle-runtime.md#bundle-api-endpoints)). `{bundle}` is
the bundle id percent-encoded as one path segment
(`hya-extra%2Ftoken-summary`).

- `GET /v1/bundle-apis` → `{ "apis": [{ "bundle", "api", "method", "scope",
  "path", "description"?, "requestSchema"?, "responseSchema"? }] }` (rpc
  `BundleApi.ListBundleApis`), sorted by bundle then endpoint id. `path` is
  the declared template (`/items/{id}`); the schemas are the declared JSON
  Schema documents.
- Session scope — `GET|POST|PUT|PATCH|DELETE
  /v1/sessions/{session}/bundles/{bundle}/{path…}` (rpc
  `BundleApi.InvokeSessionBundleApi`). The session must exist; the bundle
  process may read its data through a read-only capability.
- Global scope — `GET|POST|PUT|PATCH|DELETE /v1/bundles/{bundle}/api/{path…}`
  (rpc `BundleApi.InvokeGlobalBundleApi`), not tied to a session.

`{path…}` is the rest of the URL path, matched against the bundle's templates
for that scope (a template parameter matches one segment; `%2F` inside a
segment stays inside it). The query string reaches the process as a
string→string map; a non-empty request body must be JSON (at most 512 KiB; the
`Content-Type` header is not inspected) and an empty body is `null`. Request
headers are never forwarded.

Over HTTP the response **is** the process's answer: its status (any
`200..=599`) and its JSON body verbatim (`Content-Type: application/json`,
integers exact), or no content when it answered no body. Only host-side
failures use the error envelope above (`session_not_found`,
`bundle_api_not_found`, `bundle_api_method_not_allowed`,
`bundle_api_bad_request`, `bundle_api_failed`) — so clients that must tell
them apart from a bundle's own `404` should check for the
`{"error": {"code", "message"}}` shape.

Over gRPC the invoke rpcs take `{ session (session scope), bundle, method,
path (with its leading "/"), query, body: google.protobuf.Value }` and answer
`BundleApiResponse { bundle, api, status, content_type ("application/json" or
"" without a body), body: google.protobuf.Value }`. A process status is data:
a `404` from the bundle is an OK gRPC call with `status: 404`; only host-side
failures are gRPC errors (codes in the table above). `Value` numbers are
doubles.

```
GET /v1/sessions/hysec_.../bundles/hya-extra%2Ftoken-summary/usage?scope=session
→ 200 {"session":"hysec_...","scope":"session","generated_by":"hya-extra/token-summary","models":[...],"total":{...},"sessions":[...]}

PUT /v1/bundles/acme%2Fnotes/api/notes/todo   {"text":"ship it"}
→ 201 {"key":"todo","text":"ship it"}      (whatever the bundle answers)
```

## Terminal (PTY)

`POST /v1/pty` creates a session; `POST /v1/pty/{id}/connect-token` mints a
one-time ticket. `GET /v1/pty/{id}/connect?ticket=...` upgrades to a
WebSocket speaking the same frames as the gRPC `StreamPty` rpc:

- client → server: `{"input": "<base64>"}`, `{"resize": {"cols": 120,
  "rows": 40}}`, `{"ping": true}`
- server → client: `{"output": "<base64>"}`, `{"exit": 0}`, `{"pong": true}`

The first server frame replays the current buffer. Resize currently relies
on the shell's own TTY sizing; a runtime resize API is tracked in the
consolidation plan.

## Minimal client walkthrough

```
1. GET  /v1/health                                  → verify liveness
2. GET  /v1/bootstrap                               → config + catalogs
3. POST /v1/sessions        {agent, model, workdir} → {session: {id}}
4. GET  /v1/sessions/{id}/events/stream             → SSE subscribe
5. POST /v1/sessions/{id}/turns {prompt: {text}}    → {turn: {id, state}}
6. ... consume messageStarted / partStarted / partAppended / partReplaced / messageFinished ...
7. POST /v1/interactions/{id}/respond               → when asked
8. GET  /v1/sessions/{id}/messages                  → transcript reads
```

## Regenerating the docs

`cargo run -p xtask -- gen-api` regenerates `api-reference.md`,
`openapi.json`, and the Rust contract crate from `proto/hya/v1`. The task
fails when any rpc lacks its `// hya.http:` mapping or two rpcs claim the
same route, so documentation cannot drift from the IDL.
