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

Requires Bun 1.3 or newer and a terminal supported by OpenTUI. From a clone:

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
is open, admits the prompt as a turn, and refreshes its transcript from the
server as SSE frames arrive. For example, type `summarize this repository`,
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
| `/cancel` | Request cancellation of the turn admitted in this frontend. |
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
| `GET /v1/sessions/{id}/events/stream?sinceSeq=N` | SSE | `StreamFrame` with `event` or `resync` |
| `GET /v1/sessions/{id}/events?sinceSeq=N` | No body | `ListEventsResponse` on stream resync |
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

The transcript is read from projected `MessageInfo.parts` after event
notifications. The TUI does not derive a competing durable state model from
SSE deltas. List requests follow the server's `page.nextCursor` using the
`page.cursor` and `page.limit` query keys. `GET /v1/auth` is an unpaginated
names-only list. The generic `/api` command sends the supplied JSON unchanged to
the named `/v1` route; its full request and response schemas are in the
[generated API reference](protocol/api-reference.md).
For non-2xx responses with an empty or invalid JSON body, the frontend reports
`METHOD /v1/path: HTTP <status> <status text>`; a structured error envelope
continues to show its code and message.

## Verify locally

```sh
cd packages/hya-tui
bun run typecheck
bun test
```
