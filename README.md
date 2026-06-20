# yaca

A multi-agent coding agent built in Rust as an event-sourced workspace of focused
crates. A `SessionEngine` runs the agent turn loop over a normalized provider
layer; goal mode and loop mode are governed by an independent evaluator so the
engine — never the worker — owns the stop decision.

## Status

The full stack compiles, lints clean (`clippy -D warnings`), and the workspace
test suite passes. `yaca` talks to **real models** by reusing opencode's provider
config (OpenAI-compatible + Anthropic routes over streaming SSE; see
[Configuration](#configuration)), and falls back to an offline echo provider when
no config is present.

## Crates

| Crate | Responsibility |
|-------|----------------|
| `yaca-proto` | Ids, `Event`/`Envelope`/`Message`/`Part`, the projection reducer, HTTP DTOs |
| `yaca-provider` | Provider trait, protocol decoders (OpenAI, Anthropic), router, preflight, `FakeProvider` |
| `yaca-tool` | Permission plane + builtin tools (read, write, edit, glob, grep, shell) |
| `yaca-store` | SQLite event log, projection cache, token ledger, replay |
| `yaca-core` | `SessionEngine`, turn loop, event bus, goal/loop engines, team plane, categories, worktrees |
| `yaca-server` | axum HTTP + SSE over the engine |
| `yaca-client` | reqwest client for the server API |
| `yaca-tui` | ratatui chat render (scrollable transcript, status bar, input box) |
| `yaca-cli` | the `yaca` umbrella binary — interactive TUI + headless subcommands |

## Build

```sh
cargo build --workspace
```

## Usage

The default entry is an interactive chat TUI (crossterm + ratatui) — the main way
to use yaca:

```sh
# Launch the interactive TUI (just run `yaca` with no arguments).
yaca
#   type a message, Enter to send, responses stream in live,
#   PgUp/PgDn to scroll history, Ctrl-C to quit.

# Pick a specific model (else the opencode default / YACA_MODEL is used).
yaca --model claude-sonnet-4-6
```

Headless subcommands remain for scripting and automation:

```sh
# Single headless turn; prints the transcript.
yaca exec "summarize this repo"

# Headless goal mode: iterate until an independent evaluator reports the goal
# met, or the iteration cap trips.
yaca -p "make all tests pass" --max-iterations 6

# HTTP + SSE server (use :0 for an ephemeral port; empty --db is in-memory).
yaca serve --bind 127.0.0.1:8080 --db yaca.db

# Replay a persisted session's event log as JSON lines.
yaca tail-session <session-uuid> --db yaca.db
```

### HTTP API

```
POST /sessions                 -> { "session": "<uuid>" }
POST /sessions/:id/prompt      -> { "message": "<uuid>", "finish": "stop" }
GET  /sessions/:id/events      -> [ Envelope, ... ]
GET  /sessions/:id/stream      -> text/event-stream of Envelopes
```

## Architecture

- **Event-sourced.** Every turn appends `Event`s to the store; the read model is
  a deterministic projection replayed from the log, so `tail-session` and the
  server's `/events` return identical history.
- **Normalized providers.** Provider-specific protocols decode into one `Event`
  stream; a router resolves a `ModelRef` to a provider and runs capability
  preflight before streaming.
- **Engine-owned stop authority.** In goal mode a separate evaluator judges only
  the transcript; in loop mode a verifier (not the planner) is the sole judge of
  success, behind explicit iteration/no-progress caps.
- **Isolation.** Team members run in their own sessions with scoped permissions
  and per-agent worktrees.

## Configuration

yaca reuses **opencode's** config — no separate setup. It reads
`~/.config/opencode/opencode.json` (honoring `$XDG_CONFIG_HOME`) and, for each
entry under `provider`, builds a real HTTP route:

- `npm` `@ai-sdk/openai-compatible` → OpenAI Chat Completions (`Authorization: Bearer`)
- `npm` `@ai-sdk/anthropic` → Anthropic Messages (`x-api-key` + `anthropic-version`)
- `options.baseURL` + `options.apiKey` (literal or `{env:VAR}` / `{file:path}`) + the
  `models` keys define the route and which model ids it serves.

Responses stream over SSE. API keys are held in `SecretString`, sent as sensitive
headers, and never logged. Redirects are disabled so an auth header can't follow a
3xx to another host.

Select the model with `--model <id>` or `YACA_MODEL`; otherwise yaca prefers a
`sonnet` model, then the first available. With no usable opencode config, yaca
falls back to an offline echo provider so the stack still runs.

## Development

The per-change quality gate:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
