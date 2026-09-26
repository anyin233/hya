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

A JSON body that does not decode into the request message — malformed JSON,
a wrong field type, an unknown field, or a number outside the field's range
(for example `"contextLimit": -1` or a value above `4294967295` for a
`uint32`) — is `invalid_argument` with this same `{"error": ...}` shape,
never a plain-text 422. A body over the transport limit stays HTTP 413.

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

## Archived sessions

A root session can be archived to hide it from the default session list
without deleting it. The TUI archives its session when you quit it
gracefully; `--resume` brings it back. Archiving is only a flag: a turn that
is running keeps running and finishes, and the transcript stays readable.

- **Archive / unarchive:** `PATCH /v1/sessions/{id}` (`UpdateSession`) with
  `{"archived": true}` or `{"archived": false}`. Both are idempotent;
  archiving an archived session keeps its first `archivedAt`. Only root
  sessions are archived: archiving a subagent child session is
  `invalid_argument` (unarchiving one is a no-op). Subagent sessions are
  never archived themselves.
- **Implicit unarchive:** admitting a new prompt, command (including
  `/workflow`), or shell turn on an archived session unarchives it before
  the turn's user message is recorded. Engine-internal continuations (goal
  and loop rounds, subagent wake-ups) do not.
- **Read:** `SessionInfo.archived` (`bool`) and `SessionInfo.archivedAt`
  (timestamp; unset when not archived).
- **List:** `GET /v1/sessions` (`ListSessions`) leaves archived root
  sessions out. `includeArchived=true` lists them too; `archivedOnly=true`
  lists only them. Filtering happens before pagination.
- **Stream:** each change is a durable `sessionUpdated` event with only
  `archived` set (`true` or `false`), on the per-session stream and on the
  global stream (not with `interactionsOnly`), so other clients update
  live.
- **Fork:** a fork of an archived session is not archived.
- **Durable events:** `session_archived` (with an epoch-millisecond stamp)
  and `session_unarchived`; see
  [event-model.md](../architecture/event-model.md#session-lifecycle).

```sh
curl -X PATCH localhost:3250/v1/sessions/hysec_... -d '{"archived": true}'
curl 'localhost:3250/v1/sessions?includeArchived=true'
curl 'localhost:3250/v1/sessions?archivedOnly=true'
```

```json
{ "event": { "seq": "88", "session": "hysec_...", "sessionUpdated": { "archived": true } } }
```

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
crossed its threshold mid-turn. `strategy` is a stable snake_case name:
`native` (provider-native compaction), `local_summarizer` (a model-written
summary; also every manual compaction), `snap_compact` (a local dense
archive, no model call), or `handoff` (a model-written handoff document).
The record is in `ListEvents` too, so a transcript read can place the
divider.

`POST /v1/sessions/{id}/compact` (`CompactSession`) answers
`{ compactedUntilSeq, strategy }` with the same `strategy` name the recorded
`compactionApplied` carries (`local_summarizer`). The request's `untilSeq` is
deprecated and ignored: a manual compaction always folds the whole
transcript at the head, and `compactedUntilSeq` reports the watermark
reached. Without a configured summarizer the call is `503 unavailable`.

```json
{ "event": { "seq": "40", "session": "hysec_...", "compactionApplied": { "untilSeq": "40", "strategy": "local_summarizer", "message": "msg_...", "foldedCount": 12, "manual": true } } }
```

## Prompt attachments (images)

A prompt turn can carry images for the model: a screenshot, a diagram, a
photo of a whiteboard. `PromptTurn.attachments` is a list of
`PromptAttachment`:

| Field | Type | Meaning |
| --- | --- | --- |
| `name` | string (required) | File name shown in the transcript and sent to the model. |
| `mime` | string | `image/png`, `image/jpeg`, `image/gif`, or `image/webp`. Empty: detected from the bytes. When set it must match the bytes. |
| `data` | bytes | The image. protojson: standard base64 (not a `data:` URL). |
| `path` | string | Where the client read the file; recorded for display, never read by the server. |

```sh
curl -X POST localhost:3250/v1/sessions/hysec_.../turns -d '{
  "prompt": {
    "text": "What is wrong with this layout?",
    "attachments": [
      {"name": "screen.png", "mime": "image/png", "data": "iVBORw0KGgo...", "path": "/tmp/screen.png"}
    ]
  }
}'
```

Limits and errors. Every failure below is `invalid_argument` (HTTP 400,
gRPC `InvalidArgument`) and admits nothing: no user message, no model round.

| Check | Limit |
| --- | --- |
| type | PNG, JPEG, GIF, WebP only, checked against the file signature; a declared type that differs from the bytes is refused |
| one attachment | 10 MiB of image bytes (`MAX_ATTACHMENT_BYTES`), not empty, `name` not empty |
| one turn | 20 MiB in total (`MAX_TURN_ATTACHMENT_BYTES`) |
| model | the turn's model must not declare `image_input: false` (`ModelSummary.imageInput`, from the config `modalities.input` of the model entry — see [Configuration](../configuration.md)); unknown support is allowed |

The `CreateTurn` JSON body may be up to 32 MiB over HTTP
(`hya_server::MAX_TURN_REQUEST_BYTES`: the 20 MiB budget after base64 plus
the rest of the request) and the gRPC message up to 24 MiB
(`MAX_TURN_GRPC_MESSAGE_BYTES`, binary bytes); a larger request is refused
by the transport before validation (HTTP 413, gRPC `OutOfRange`). Other
routes keep the 2 MB default body limit.

The user message and its images are recorded in one store transaction, so
the prompt is never visible (to the model or a reader) without its images.
Every later round and turn of the session sends the images again as part of
the history, until a compaction folds that message away.

**Transcript.** `ListMessages` / `GetMessage` list each image as a part after
the prompt text:

```json
{ "id": "part_...", "attachment": { "name": "screen.png", "mime": "image/png", "path": "/tmp/screen.png", "size": "48213" } }
```

`AttachmentPart.data` is always empty in transcript reads and on the stream:
the bytes are stored once, in the session's blob table, and only go to the
model. A listing therefore stays small no matter how many images a session
has; there is no route that returns the bytes. `size` is the byte count and
`path` is what the client sent (omitted when empty).

**Stream.** The durable `partsAdded { message, parts }` frame carries the
same parts right after the user message's text part and before its
`messageFinished`; append them to the message.

```json
{ "event": { "seq": "14", "session": "hysec_...", "partsAdded": { "message": "msg_...", "parts": [ { "id": "part_...", "attachment": { "name": "screen.png", "mime": "image/png", "size": "48213" } } ] } } }
```

A [fork](#fork) keeps the images of the copied messages.

## Revert and redo

`POST /v1/sessions/{id}/revert` (`RevertSession`) is `/undo`: it hides a
user message and every later message, and restores the files their tool
calls changed (see [Runtime — File snapshots and
revert](../architecture/runtime.md#file-snapshots-and-revert) for what is
captured and the size limits). `{"undo": true}` is `/redo`: the hidden
messages come back and the files are written back to their state before the
revert. The next prompt or shell turn **commits** a pending revert: the
hidden messages are dropped for good, the model never sees them, and undo is
refused from then on.

| Request | Effect |
| --- | --- |
| `{}` | revert the last visible user message; repeat to go further back |
| `{"messageId": "msg_..."}` | revert to that user message (it and every later message are hidden) |
| `{"undo": true}` | undo the pending revert (`messageId` ignored) |

The response is `{ session: SessionInfo, files: [RevertedFile] }`.
`SessionInfo.revert` is set while a revert is pending: `{ messageId, text
(the reverted prompt, e.g. to refill the composer), hiddenMessages, files }`.
`ListMessages`/`GetMessage` leave the hidden messages out (they are not
flagged, they are absent). Each `RevertedFile` is `{ path, action, reason }`
with `action` `restored`, `deleted` (the file did not exist then, so it was
removed), `unchanged`, `skipped` (content not kept: `reason` `too_large`,
`session_cap`, `snapshot_budget`, `unreadable`), or `failed` (`reason` is
the write error).

| Error | When |
| --- | --- |
| `session_busy` (409) | a turn is running or being admitted on the session |
| `invalid_argument` (400) | no user message to revert; `messageId` is not a user message or is already reverted; `undo` with nothing pending; a nonzero `untilSeq` (deprecated, unsupported) |
| `not_found` (404) | `messageId` is not in the session |
| `session_not_found` (404) | unknown session |

The session stream carries each revert and undo as durable
`sessionReverted { messageId, undone, files }` (`messageId` empty and
`undone: true` for an undo). Re-read the session and its messages on it; a
later `messageStarted` means the revert was committed.

```json
{ "event": { "seq": "57", "session": "hysec_...", "sessionReverted": { "messageId": "msg_...", "files": [ { "path": "/repo/a.txt", "action": "restored" } ] } } }
```

```sh
curl -X POST localhost:3250/v1/sessions/hysec_.../revert -d '{}'
curl -X POST localhost:3250/v1/sessions/hysec_.../revert -d '{"undo": true}'
```

## Fork

`POST /v1/sessions/{id}/fork` (`ForkSession`) creates a new root session
with a copy of the source's visible transcript (never messages hidden by a
pending revert):

| Request | The fork holds |
| --- | --- |
| `{}` | every message (the head, last message included) |
| `{"messageId": "msg_..."}` | the messages strictly before that user message; the response's `promptText` is that message's text, to prefill the composer |
| `{"untilSeq": "<seq>"}` | the messages whose `messageStarted` has `seq <= untilSeq` |

The response is `{ session: SessionInfo, promptText }`; the new session's
`SessionInfo.forkedFrom` is `{ session, messageId }` (`messageId` empty for a
head or `untilSeq` fork). A `messageId` that is not a user message is
`invalid_argument`; one not in the source is `not_found`. Copied messages get
new ids; the source's file snapshots are not copied, so reverting a copied
turn in the fork restores no files. Prompt images of copied messages are
copied with them (see [Prompt attachments](#prompt-attachments-images)).

The fork is titled `<source title> (fork)` — the source's session id stands
in for a missing or default title, and a fork of a fork keeps the single
`(fork)` suffix (`Plan the work (fork)`, not `… (fork) (fork)`). Automatic
titling renames only sessions whose title is missing or a default, so it
never renames a fork; rename one with `PATCH /v1/sessions/{id}`
(`{"title": "..."}`).

## Live and durable frames

Stream events come in two kinds:

- **Durable** events carry their log `seq` (a string, `"12"`). They are in
  the event log, `ListEvents` replays them, and the projection
  (`ListMessages`, `GetMessage`) folds them.
- **Live-only** events carry no `seq` (it is `0`, which protojson omits).
  They are never persisted and `ListEvents` never returns them: the
  assistant text of an in-flight provider round, the pending
  interaction frames (`permissionRequested`, `questionRequested`,
  `interactionResolved`; `GET /v1/interactions` is their listing), and the
  process-wide `catalogUpdated` notice.

**`catalogUpdated`.** When the provider/model catalog changes — a provider
is added, edited, or refreshed, a key is set or removed, or startup model
discovery finishes — every live stream (global and each session stream)
receives one live-only `catalogUpdated` frame with an empty payload and an
empty `session`. Re-read `GET /v1/models` / `GET /v1/providers`.

```json
{ "event": { "timeRecorded": "2026-09-26T10:00:00Z", "catalogUpdated": {} } }
```

**Interactions-only global stream.** `GET /v1/events/stream?interactionsOnly=true`
(gRPC `StreamGlobalEventsRequest.interactions_only`) delivers only the live
interaction frames of every session plus `catalogUpdated`; every session's
engine events (text, tools, messages, status) and their `resync` frames are
left out. A client that follows its open session on the session stream uses
it to see the other sessions' asks without receiving their live text.

While a provider round streams, each assistant text part arrives live as
`partStarted` (`kind: "text"`), one `partAppended` per delta, and
`partCompleted`. When the round's stream ends, the durable log records the
same part once, with the **same** message and part ids: `partStarted`,
`partReplaced` (`text` = the final full text), `partCompleted`. Reasoning
deltas and user-message text are durable `partStarted` / `partAppended` /
`partCompleted` events; tool-call arguments are durable `partStarted` /
`partAppended` followed by `toolStateChanged` (see [Tool calls](#tool-calls)).
A prompt's image attachments arrive as one durable `partsAdded` (see
[Prompt attachments](#prompt-attachments-images)).
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
them into the running engine, and emits a live `catalogUpdated` frame on
every v1 event stream (see [Live and durable frames](#live-and-durable-frames))
— no restart. The effective model list of a provider is its cached
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
  "displayName": "Alpha",
  "reasoning": true,        // declared by metadata; absent when unknown
  "auth": "AUTH_STATUS_CREDENTIALED",
  "contextLimit": "64000",  // uint64 → strings; absent when unknown
  "outputLimit": "4096",    // absent when unknown
  "source": "override" }    // remote | config | override | offline

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
- `ModelSummary` reports only the metadata the model publishes (config
  `models:` fields, else the remote model list). A model without a known
  context window omits `contextLimit` (the runtime then sizes compaction
  against a 200000-token fallback); without a known output limit it omits
  `outputLimit`; without a reasoning claim it omits `reasoning` (the route
  still accepts its provider family's effort variants). `reasoning: false`
  is an explicit claim (`reasoning: false` in config).
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

## Saved permission rules

An "allow always" reply to a permission ask is saved as a rule and applies
at once to every session. Saved rules are process-wide (not per project or
directory; the `directory` field is accepted and ignored), survive a server
restart (they are reloaded into the permission plane at startup), and a
deleted rule stops applying immediately: the next matching call asks again.

| Call | HTTP | Answer |
| --- | --- | --- |
| `Interactions.ListSavedRules` | `GET /v1/permissions/rules` | `{rules: [SavedRule], page}`, stable id order |
| `Interactions.DeleteSavedRule` | `DELETE /v1/permissions/rules/{rule}` | `{}` (also for an unknown id) |

```jsonc
// SavedRule
{ "id": "psv_per_...",
  "permission": "RULE_PERMISSION_ALLOW",   // always ALLOW for saved grants
  "tool": "bash",                          // exact tool or MCP tool name, `bash`
                                           // for a command, else the action name
  "pattern": "cargo test",                 // the exact command for `bash`, `*` for an
                                           // action-wide grant, empty for a tool grant
  "timeCreated": "2026-09-26T10:00:00Z" }  // absent for rules saved before creation
                                           // times were recorded
```

## Working-tree diff

`GET /v1/vcs/diff` (`Project.GetVcsDiff`) answers `{ diff }`: git's unified
patch of the scope directory's working tree against `HEAD` (`git diff HEAD`)
followed by a patch for every untracked file; empty outside a git
repository. `paths` restricts it to those files or directory prefixes
(pathspecs relative to the scope directory); over HTTP repeat the key:
`/v1/vcs/diff?paths=src/main.rs&paths=docs`. A path that is absolute or
contains `..` is `invalid_argument`. `raw` is accepted and ignored.

## MCP servers

`POST /v1/mcp` (`Mcp.AddMcpServer`) stores (or replaces) one server —
`{name, command: {command, args, env} | url: {url}, enabled?}` — and
answers its `McpServerStatus {name, state, tools, error, authRequired}`.
The server is stored even when it cannot start: a spawn or handshake
failure answers `200` with `state: "MCP_SERVER_STATE_FAILED"` and `error`.
`enabled: false` stores it without connecting
(`MCP_SERVER_STATE_DISCONNECTED`). Re-adding an unchanged config does not
reconnect. `POST /v1/mcp/{name}/connect` enables and connects a stored
server (a failure is again a `FAILED` status; an unknown name is
`404 not_found`), `POST /v1/mcp/{name}/disconnect` disables it, and
`GET /v1/mcp` lists every server's status.

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
5. POST /v1/sessions/{id}/turns {prompt: {text, attachments?}} → {turn: {id, state}}
6. ... consume messageStarted / partStarted / partAppended / partReplaced / messageFinished ...
7. POST /v1/interactions/{id}/respond               → when asked
8. GET  /v1/sessions/{id}/messages                  → transcript reads
```

## Regenerating the docs

`cargo run -p xtask -- gen-api` regenerates `api-reference.md`,
`openapi.json`, and the Rust contract crate from `proto/hya/v1`. The task
fails when any rpc lacks its `// hya.http:` mapping or two rpcs claim the
same route, so documentation cannot drift from the IDL.
