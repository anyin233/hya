# Server and Client

The server lives in [`../../crates/yaca-server`](../../crates/yaca-server). It
wraps `SessionEngine` with Axum routes and streams live envelopes over SSE.

## App State

`AppState` contains:

- shared `SessionEngine`
- process-level `AgentSpec`

`POST /sessions` records the requested session metadata. `POST /prompt` runs the
turn with the server's configured `AgentSpec`.

## Routes

| Method | Path | Request | Response |
| --- | --- | --- | --- |
| `POST` | `/sessions` | `CreateSessionRequest` | `CreateSessionResponse` |
| `POST` | `/sessions/:id/prompt` | `PromptRequest` | `PromptResponse` |
| `GET` | `/sessions/:id/events` | optional `since_seq` query | `Vec<Envelope>` |
| `GET` | `/sessions/:id/stream` | none | SSE stream of envelopes |

Session ids in URL paths are raw UUIDs. Invalid ids return `400 Bad Request`.
Runtime errors are currently returned as `500 Internal Server Error` with the
error text as the body.

## Create Session

`POST /sessions` accepts:

```json
{
  "agent": "build",
  "model": "claude-sonnet-4-6",
  "workdir": ".",
  "parent": null
}
```

and returns:

```json
{
  "session": "..."
}
```

The `session` value is serialized by the `SessionId` type.

## Prompt

`POST /sessions/:id/prompt` accepts:

```json
{
  "text": "summarize this repository"
}
```

The server admits the prompt, runs one assistant turn, and returns the user
message id plus finish reason.

## Events

`GET /sessions/:id/events` replays stored envelopes for a session. Use
`?since_seq=<n>` to receive only envelopes whose sequence is greater than `n`.

## Stream

`GET /sessions/:id/stream` subscribes to the engine event bus and emits SSE
events for the requested session. If the broadcast receiver lags, the server
emits an SSE event named `resync`; clients should use the events endpoint with
their last seen sequence to catch up.

## Client Crate

[`../../crates/yaca-client/src/lib.rs`](../../crates/yaca-client/src/lib.rs)
provides a typed reqwest wrapper:

- `create_session`
- `prompt`
- `events`

The current interactive TUI runs in-process through `yaca-cli`; the client crate
is the integration surface for code that talks to a running server process.
