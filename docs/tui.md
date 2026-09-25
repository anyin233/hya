# OpenTUI frontend

The `packages/hya-tui` frontend is a basic terminal client for a running
`hya serve` process. It uses OpenTUI for display and input while the
backend remains the owner of sessions, event history, tool execution, and
permissions. The screen shows a session list, the selected transcript, and
pending interactions. Models, Workflows, and saved provider keys have dedicated
views; the API command view exposes the other HTTP/JSON operations in `hya.v1`.
Tab completes slash commands using the TUI and server command catalogs.
One persistent instruction line stays below the input at the bottom of the
screen and changes with the current view.
If a backend predates the saved-key list endpoint, the main TUI still opens and
shows that key listing needs a backend restart with an updated binary.

## Start it

Requires Bun 1.4.2 (the version the repository pins; the Solid setup is verified on it) and a terminal supported by OpenTUI. From a clone:

```sh
cd packages/hya-tui
bun install --frozen-lockfile
```

Run these in separate terminals from the repository root:

```sh
cargo run --locked -p hya-backend --bin hya -- serve --bind 127.0.0.1:8080 --db "$HOME/hya-sessions.db"
bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080 --dir "$PWD"
```

`--server` is the backend base HTTP URL (default `http://127.0.0.1:8080`).
`--dir` is the absolute directory scope sent as `x-hya-directory` (default:
the frontend process's working directory). `--help` prints the launch syntax.
The backend's offline echo model is sufficient for a first run; configure a
provider in the backend for live model calls.

Type a plain prompt and press Enter. The frontend creates a session when none
is open, admits the prompt as a turn, and streams the reply into the
transcript as it arrives (see [Streaming, queued prompts, and turn
status](#streaming-queued-prompts-and-turn-status)). For example, type `summarize this repository`,
then `/models` to inspect available routes, and `/open 1` to return to the
first session. Press Ctrl+C to exit and restore the terminal.

To set a provider API key, type `/key set anthropic`, paste the key into the
concealed prompt, and press Enter. The prompt draws bullets only and clears its
buffer after submission; Esc cancels. `/keys` lists saved provider IDs, and
`/key remove anthropic` deletes that provider's saved credential. The backend
stores the key in its user auth directory; it never sends existing key values
back to the TUI. Configure that provider's model route in the backend config,
then restart the backend after adding or removing a key so the route resolves
the new credentials. OAuth login remains available through the backend CLI.
After typing `/keys`, read the bottom row: it shows `/key set <provider>` to
add or replace a key and `/key remove <provider>` to delete one. During
concealed entry, the row changes to `Paste API key · Enter saves · Esc cancels`.

If `/keys` says key listing is unavailable, restart the backend with hya
0.41.0 or newer and run the same frontend command again. For example, a
frontend on `127.0.0.1:22103` can reconnect after restarting the backend on
that port; sessions and other main views remain available while its older
backend is running.

## Commands and keys

| Input | Effect |
| --- | --- |
| Plain text + Enter | Admit a prompt in the current session; create one if needed. |
| `/new [agent] [model]` | Create a session in `--dir`, using the first visible agent and its model by default. |
| `/sessions`, `/open <id or number>` | Refresh or switch sessions. |
| `/models`, `/model <provider/model>` | View catalog or change the selected session model. |
| `/keys` | List configured providers and provider IDs with saved credentials; never display key values. |
| `/key set <provider>`, `/login <provider>` | Open concealed entry for a provider API key; Enter saves, Esc cancels. |
| `/key remove <provider>` | Delete the provider's saved credential. |
| `/workflows`, `/workflow select <name>`, `/workflow run [name]` | View sources and selected state; select or start a Workflow in the selected session. |
| `/interactions` | View pending permissions and questions. |
| `/approve <id>`, `/deny <id>` | Respond to a permission request for this run only (`persist: false`). |
| `/answer <id> <text>` | Answer a question request. |
| `/cancel` | Request cancellation of the turn admitted in this frontend; the status line then shows `Cancelled · Ready`. |
| `/refresh` or Ctrl+R | Reload sessions, messages, interactions, models, and Workflows. |
| `/api` | List the HTTP operations from the generated OpenAPI catalog. |
| `/api METHOD /v1/path [JSON]` | Send a scoped HTTP/JSON request and show its JSON response. |
| `/help` | Show command help. |
| Tab | Complete a slash command or supported argument; repeat Tab to cycle matches. |

The bottom instruction row is separate from the status message above the
input. Status updates and completion suggestions can change without erasing
the next-step instruction.

Other slash commands are forwarded to the backend as `CommandTurn`s, so
custom commands from the server catalog remain usable in this frontend. Tab
suggestions also use that catalog. Argument completion covers agents, sessions,
models, Workflows, pending interaction IDs, provider IDs, saved key names, and
HTTP operations from the generated OpenAPI catalog. Suggestions are refreshed
with `/refresh` or Ctrl+R.

The API command accepts `GET`, `POST`, `PUT`, `PATCH`, and `DELETE`; the optional
body must be JSON. `GET` has no body. Include query parameters directly in the
path. For example:

```text
/api GET /v1/health
/api GET /v1/sessions
/api PATCH /v1/sessions/hysec_... {"title":"Review"}
```

It only accepts paths beginning `/v1/`, so a command cannot redirect the
client to another origin. The catalog marks server-streaming operations with
`[stream]`; the one-shot API command does not consume those streams. Session
SSE is connected automatically when a session is open. PTY WebSocket sessions
need a WebSocket client; the command view can still call their JSON setup
routes. See the [protocol guide](protocol/README.md) for those frames.

## Streaming, queued prompts, and turn status

The assistant reply appears chunk by chunk while the model streams it. When
the reply is complete, the transcript shows the server's stored copy of it;
the text does not repeat or flicker when that happens.

You can type the next prompt while a turn is running. Press Enter and the
prompt appears dimmed at the end of the transcript under a `user · queued`
header. The status line counts the waiting prompts
(`Running · msg_… · 1 queued`). When the running turn ends, the frontend sends
the oldest queued prompt; several queued prompts go one per turn, in the
order you typed them. The server has no prompt queue of its own. It rejects
a prompt with `409 session_busy` while a turn runs, and it releases the
session shortly after the reply finishes. So the frontend retries a busy
prompt a few times with a short backoff (about 100 ms growing to 1 s). If the
session is still busy after that (for example, another client started a
turn), the prompt stays queued and is sent after the next turn end seen on the
stream. Opening another session drops the queued prompts of the previous one.
A queued prompt is still sent after a cancelled or failed turn.

The status line above the input shows the turn state:

| Status | Meaning |
| --- | --- |
| `Sending prompt…` | The prompt is being admitted (`CreateTurn` in flight). |
| `Session busy · retrying (N)` | The server answered `409 session_busy`; the prompt is retried. |
| `Running · <turn id>[ · N queued]` | The turn runs; `N` prompts wait. |
| `Session busy · N queued prompt(s) wait(s) for the running turn` | Retries ran out; the prompts wait for the next turn end. |
| `Ready` | The turn finished. `Ready · reply stopped at the length limit` when the model hit its output limit. |
| `Cancelled · Ready` | The turn was cancelled (`/cancel`). |
| `Error · <code>: <message>` | The turn failed, for example `Error · provider_error: http status 400: …`. `Error · turn failed` when the backend recorded no error text. |

A failed assistant message also shows its error in the transcript, as a line
under its `assistant · error` header:

```text
assistant · error
error · provider_error: http status 400: bad request
```

## Interface definitions

The frontend uses the existing HTTP/JSON+SSE transport. Every request carries
`x-hya-directory: <absolute --dir path>`; JSON uses protojson lower camel case,
string encoded 64-bit values, and the error envelope documented in the
[protocol guide](protocol/README.md). These are the first-class calls:

| Method and route | Request | Response read by the TUI |
| --- | --- | --- |
| `GET /v1/bootstrap` | No body | `Bootstrap` (`location`, `agents`, `models`, `interactions`) |
| `GET /v1/sessions` | No body | `ListSessionsResponse.sessions: SessionInfo[]` |
| `POST /v1/sessions` | `{agent: string, model: string, workdir: string}` | `CreateSessionResponse.session: SessionInfo` |
| `GET /v1/sessions/{id}` | No body | `SessionInfo` |
| `PATCH /v1/sessions/{id}` | `{model: string}` | `SessionInfo` |
| `GET /v1/sessions/{id}/messages` | No body | `ListMessagesResponse.messages: MessageInfo[]` |
| `POST /v1/sessions/{id}/turns` | `{prompt: {text: string}}` | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{command: {command: string, arguments: string}}` for other slash commands | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns/{turn}/cancel` | `{}` | `CancelTurnResponse` |
| `GET /v1/sessions/{id}` | No body | `SessionInfo.lastSeq` when a session is opened (the stream's first `sinceSeq`). |
| `GET /v1/sessions/{id}/events/stream?sinceSeq=N` | SSE | `StreamFrame` with `event` or `resync`; `N` is the last applied durable seq. |
| `GET /v1/sessions/{id}/events?sinceSeq=N&limit=500` | No body | `ListEventsResponse.events` / `nextSeq`, paged, to fill the gap after each stream (re)connect and `resync`. |
| `GET /v1/interactions` | No body | `ListInteractionsResponse.interactions: Interaction[]` |
| `POST /v1/interactions/{id}/respond` | `{permission: {allowed: boolean, persist: false}}` or `{question: {answer: string}}` | `RespondInteractionResponse.applied` |
| `GET /v1/models` | No body | `ListModelsResponse.models: ModelSummary[]` |
| `GET /v1/providers` | No body | `ListProvidersResponse.providers: ProviderSummary[]` for key suggestions. |
| `GET /v1/commands` | No body | `ListCommandsResponse.commands: CommandSummary[]` for slash completion. |
| `GET /v1/auth` | No body | `ListProviderAuthResponse.providerIds: string[]` (saved provider IDs only; empty field omitted). A 404 marks key listing unavailable without blocking startup. |
| `PUT /v1/auth/{provider_id}` | `{apiKey: string}` | `SetProviderAuthResponse.status: AuthStatus`; key value is sent only to the backend. |
| `DELETE /v1/auth/{provider_id}` | No body | `RemoveProviderAuthResponse` (empty). |
| `GET /v1/workflows` | No body | `ListWorkflowsResponse.workflows: WorkflowSummary[]` |
| `GET /v1/sessions/{id}/workflow` | No body | `WorkflowState` |
| `POST /v1/sessions/{id}/workflow` | `{select: {name: string}}` or `{run: {name: string}}` | `SubmitWorkflowCommandResponse` |

The one-row footer sits directly below the input panel. Its content is selected
from the current view; it makes no HTTP request:

| View or state | Bottom instruction |
| --- | --- |
| Chat | `Enter a prompt · /new creates a session · /help lists commands` |
| Models | `Next: /model <provider/model> to switch this session · /help` |
| Workflows | `Next: /workflow select <name> or /workflow run [name]` |
| Interactions | `Next: /approve <id>, /deny <id>, or /answer <id> <text>` |
| Saved keys | `Next: /key set <provider> to add · /key remove <provider> to delete · Tab completes` |
| Saved keys when `GET /v1/auth` is unavailable | `Next: restart backend 0.41.0+ to list saved keys · /help` |
| Concealed key entry | `Paste API key · Enter saves · Esc cancels` |
| API | `Next: /api GET /v1/health · /help for command syntax` |
| Help | `Enter a prompt or choose a /command · Tab completes` |

### Stream frames and the transcript

The server projection (`ListMessages`, `MessageInfo.parts`) is the
authoritative transcript. Stream frames feed a transient overlay that shows
what the projection cannot show yet, mainly the live text of the in-flight
round. The overlay is never persisted and is rebuilt from the stream. The
rules follow the protocol guide's
[Live and durable frames](protocol/README.md#live-and-durable-frames):

| Frame (`StreamEvent` field) | Kind | Effect in the TUI |
| --- | --- | --- |
| `messageStarted {message, role}` | durable | Overlay message with its role; projection re-read (debounced 120 ms). |
| `partStarted {message, part, kind}` (`text`, `reasoning`) | live or durable | Overlay part. A part id the overlay already has is not a new part. |
| `partAppended {message, part, textDelta}` | live (assistant text) or durable (reasoning, tool arguments, user text) | Appends `textDelta` to the part. No projection re-read. |
| `partReplaced {message, part, text}` | live (plugin rewrite) or durable (end of round) | Sets the part's whole text, replacing the live deltas. |
| `partCompleted {message, part}` | live or durable | No overlay change; a durable one triggers a projection re-read. |
| `errorReported {message, code, errorMessage}` | durable | Stored as the message's error. Shown in the transcript and, at turn end, in the status line. |
| `messageFinished {message, finish, cause}` | durable | The turn ends at the first assistant `messageFinished` after the turn's user message whose `finish` is not `FINISH_REASON_TOOL_CALLS`. Then the projection is re-read. |
| `permissionRequested`, `questionRequested`, `interactionResolved` | live | Pending list re-read. |
| `resync {lastSeq}` | — | Live parts that were mid-stream stop taking deltas until their durable `partReplaced`; `ListEvents` fills the gap; the projection is re-read. |

- **Sequence numbers.** The client keeps the last applied durable `seq` as a
  decimal string and compares with `BigInt`, so 64-bit values stay exact. A
  durable frame at or below it is a duplicate and is ignored. Live frames have
  no `seq` and are always applied.
- **(Re)connect.** The stream is subscribed with
  `sinceSeq = last applied seq`. Before any frame is read, `ListEvents` pages
  are replayed through the same fold. The stream does not replay history, so
  this fills the gap. After a reconnect, the projection is re-read too. A
  prompt is admitted only once the stream is subscribed (up to 3 s wait), so
  no frame of its turn is missed.
- **Handover.** The displayed transcript is the projection with the overlay
  merged by message id and part id. The overlay's text wins for a part that is
  still streaming. Overlay parts and messages that are not in the projection
  yet follow the projected ones. A projected message with a `finish` is shown
  exactly as projected, and the overlay drops it in the same store update. The
  live text and the durable text are identical, so the handover does not
  flicker. Projection reads complete in order: an older read never replaces a
  newer one.
- **Turn id.** `CreateTurn` returns the user message id as `TurnInfo.id`. It
  is used for `/cancel` and to find the turn's end. A reply that finishes
  before `CreateTurn` returns ends the turn as soon as the response arrives.
- **Session switch.** Opening a session aborts the old stream and resets the
  overlay, the prompt queue, and the turn state. Frames of any other session
  are ignored.
- **Rendering cost.** Frames are folded at once, but the overlay is published
  to the store at most once per 16 ms. A fast delta stream therefore renders
  about once per display frame, not once per chunk. Formatted message text is
  cached per message object. Projected messages and unchanged overlay
  messages keep their identity, so a delta re-formats only the message it
  changed.

List requests follow the server's `page.nextCursor` using the
`page.cursor` and `page.limit` query keys. `GET /v1/auth` is an unpaginated
names-only list. The generic `/api` command sends the supplied JSON unchanged to
the named `/v1` route; its full request and response schemas are in the
[generated API reference](protocol/api-reference.md).
For non-2xx responses with an empty or invalid JSON body, the frontend reports
`METHOD /v1/path: HTTP <status> <status text>`; a structured error envelope
continues to show its code and message.

## Code layout

The frontend is written with [`@opentui/solid`](https://github.com/anomalyco/opentui)
(Solid JSX over `@opentui/core`). `@opentui/core`, `@opentui/solid`, and
`solid-js` are pinned to exact versions in `package.json` and must move
together.

| Path | Role |
| --- | --- |
| `src/main.ts` | Entry. Registers the Solid JSX transform (`@opentui/solid/preload`), parses flags, then dynamically imports the app. |
| `src/cli.ts` | `--server`, `--dir`, `--help` parsing and the usage line. |
| `src/client.ts` | Typed v1 HTTP/JSON+SSE client (`HyaClient`, `SseDecoder`, `parseApiCommand`). |
| `src/state/store.ts` | `createAppStore()`: the single store. It holds the server projection (sessions, messages, interactions, models, agents, providers, workflows, saved key names, backend commands, stream cursor), the published streaming overlay, the prompt queue, the turn state (`running`, `turnId`), and UI state (view, status, key-entry provider and mask). Each field is a Solid signal, and only the store's mutation methods change it. |
| `src/state/overlay.ts` | `TranscriptOverlay`: the pure fold of stream frames by message and part id (seq filter, live/durable handover, `resync` handling, turn-end lookup). `mergeTranscript()` merges it over the projection. |
| `src/state/format.ts` | Pure text for each panel (header, session list, pending list, main view, the merged transcript, queued prompts, message formatting with a per-message cache). |
| `src/app/controller.ts` | `createController()`: refreshes, the session SSE loop (subscribe, `ListEvents` gap-fill, `resync`), batched overlay flushes, session creation, prompt submission, command dispatch, and concealed key entry. It writes results into the store. |
| `src/app/turns.ts` | `createTurnRunner()`: the client-side prompt queue, `409 session_busy` retry, and turn-end detection and status text. |
| `src/app/App.tsx`, `src/app/run.tsx`, `src/app/context.ts` | Root layout, renderer startup, and the `AppContext` (store, controller, server URL) that components read with `useApp()`. |
| `src/components/` | `Header`, `Panel`, `SessionsPanel`, `MainPanel`, `PendingPanel`, `StatusLine`, `Composer` (input, completion, concealed key entry), `Footer`. |
| `src/commands/` | The slash-command registry (`registry.ts`), the built-in commands (`native.ts`), and the `/help` text (`help.ts`). |
| `src/keys/bindings.ts` | The global key binding table. |
| `src/completion.ts`, `src/instructions.ts`, `src/api.ts`, `src/theme.ts` | Tab completion and `SecretEntry`, footer instructions, the OpenAPI operation catalog, and the color palette. |

The Solid transform has two parts. `bunfig.toml` preloads
`@opentui/solid/preload` for `bun test` and for `bun src/...` run inside the
package. `tsconfig.json` sets `"jsx": "preserve"` and
`"jsxImportSource": "@opentui/solid"`. Bun reads `bunfig.toml` only from the
directory it runs in, so `src/main.ts` imports the preload itself. It then
loads `.tsx` modules and `solid-js` with a dynamic `import()`. Keep static
imports in `main.ts` free of Solid code. Without the preload, Bun resolves
`solid-js` to its non-reactive server build.

To add a slash command, add a `CommandSpec` to `nativeCommandSpecs` in
`src/commands/native.ts`:

```ts
{
  name: "/title",
  description: "Rename the current session",
  argumentHint: "<text>",
  complete: ({ words, current, head }, context) => [],   // optional argument completion
  run: async ({ store, client, actions }, { args, argumentsText }) => { /* … */ },
}
```

The name becomes Tab-completable automatically. Add a line to
`src/commands/help.ts` and a row to the command table above. Unregistered
`/names` still go to the backend as `CommandTurn`s. To add a key, append a
`KeyBinding` to `src/keys/bindings.ts` and handle its action in
`components/Composer.tsx`. Do not bind a core action only to a
browser-reserved shortcut (see `docs/tui-web.md`). Ctrl+C is handled by the
renderer (`exitOnCtrlC`).

## Verify locally

```sh
cd packages/hya-tui
bun install --frozen-lockfile
bun run typecheck
bun test
```

Then check the rendered TUI in the browser from `packages/hya-tui-web`
(`bun run typecheck && bun test ./test && bunx playwright test`; see
[tui-web.md](tui-web.md)). `e2e/hya-tui.spec.ts` and
`e2e/hya-tui-commands.spec.ts` cover the layout, colors, commands, key
entry, narrow widths, and Ctrl+C. `e2e/hya-tui-streaming.spec.ts` uses the
fake model to cover streaming text, queued prompts, and the turn status
line (`Ready`, provider errors).
