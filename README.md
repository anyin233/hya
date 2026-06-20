# yaca

A multi-agent coding agent built in Rust as an event-sourced workspace of focused
crates. A `SessionEngine` runs the agent turn loop over a normalized provider
layer; goal mode and loop mode are governed by an independent evaluator so the
engine — never the worker — owns the stop decision.

## Status

The full stack compiles, lints clean (`clippy -D warnings`), and the workspace
test suite passes. The shipped `yaca` binary runs against an **offline dev
provider** so the entire pipeline (engine → provider → store → projection / HTTP /
SSE) is exercisable without API keys. Wiring real OpenAI / Anthropic providers
from config is the next step (see [Configuration](#configuration)).

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
| `yaca-tui` | ratatui three-pane render (transcript, goal/loop bars, team + permission) |
| `yaca-cli` | the `yaca` umbrella binary |

## Build

```sh
cargo build --workspace
```

## Usage

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

The dev binary uses an offline provider and requires no configuration. Real
provider, category, permission, and loop configuration (API keys, model tiers,
permission rules, loop budgets) is the documented next integration step; the
underlying engines and protocol decoders already exist and are unit-tested.

## Development

The per-change quality gate:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
