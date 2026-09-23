# OpenTUI frontend

The `packages/hya-tui` frontend is a basic terminal client for a running
`hya-backend serve` process. It uses OpenTUI for display and input while the
backend remains the owner of sessions, event history, tool execution, and
permissions. The screen shows a session list, the selected transcript, and
pending interactions. Models and Workflows have dedicated views; the API
command view exposes the other HTTP/JSON operations in `hya.v1`.

## Start it

Requires Bun 1.3 or newer and a terminal supported by OpenTUI. From a clone:

```sh
cd packages/hya-tui
bun install --frozen-lockfile
```

Run these in separate terminals from the repository root:

```sh
cargo run --locked -p hya-backend -- serve --bind 127.0.0.1:8080 --db "$HOME/hya-sessions.db"
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

## Commands and keys

| Input | Effect |
| --- | --- |
| Plain text + Enter | Admit a prompt in the current session; create one if needed. |
| `/new [agent] [model]` | Create a session in `--dir`, using the first visible agent and its model by default. |
| `/sessions`, `/open <id or number>` | Refresh or switch sessions. |
| `/models`, `/model <provider/model>` | View catalog or change the selected session model. |
| `/workflows`, `/workflow select <name>`, `/workflow run [name]` | View sources and selected state; select or start a Workflow in the selected session. |
| `/interactions` | View pending permissions and questions. |
| `/approve <id>`, `/deny <id>` | Respond to a permission request for this run only (`persist: false`). |
| `/answer <id> <text>` | Answer a question request. |
| `/cancel` | Request cancellation of the turn admitted in this frontend. |
| `/refresh` or Ctrl+R | Reload sessions, messages, interactions, models, and Workflows. |
| `/api` | List the HTTP operations from the generated OpenAPI catalog. |
| `/api METHOD /v1/path [JSON]` | Send a scoped HTTP/JSON request and show its JSON response. |
| `/help` | Show command help. |

Other slash commands are forwarded to the backend as `CommandTurn`s, so
custom commands from the server catalog remain usable in this frontend.

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
| `GET /v1/workflows` | No body | `ListWorkflowsResponse.workflows: WorkflowSummary[]` |
| `GET /v1/sessions/{id}/workflow` | No body | `WorkflowState` |
| `POST /v1/sessions/{id}/workflow` | `{select: {name: string}}` or `{run: {name: string}}` | `SubmitWorkflowCommandResponse` |

The transcript is read from projected `MessageInfo.parts` after event
notifications. The TUI does not derive a competing durable state model from
SSE deltas. List requests follow the server's `page.nextCursor` using the
`page.cursor` and `page.limit` query keys. The generic `/api` command sends the supplied JSON unchanged to
the named `/v1` route; its full request and response schemas are in the
[generated API reference](protocol/api-reference.md).

## Verify locally

```sh
cd packages/hya-tui
bun run typecheck
bun test
```
