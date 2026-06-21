# yaca

A multi-agent coding agent built in Rust as an event-sourced workspace of focused
crates. A `SessionEngine` runs the agent turn loop over a normalized provider
layer; goal mode and loop mode are governed by an independent evaluator so the
engine — never the worker — owns the stop decision.

## Status

The full stack compiles, lints clean (`clippy -D warnings`), and the workspace
test suite passes. `yaca` talks to **real models** from its own provider config
(OpenAI-compatible + Anthropic + Google routes over streaming SSE; see
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
#   PgUp/PgDn or mouse wheel to scroll, Tab completes /commands.
#   F2 or /model switches models.
#   Ctrl-C clears input, interrupts a running turn, then exits only when idle.

# Pick a specific model (else config.yaml default_model / YACA_MODEL is used).
yaca --model claude-sonnet-4-6
```

Interactive slash commands:

Type `/` plus an optional prefix and press `Tab` to complete commands. If more
than one command matches, use the list dialog and press `Enter` or `Tab`.

| Command | Behavior |
| --- | --- |
| `/model` | Open the model selector. The next prompt uses the selected model. |
| `/resume`, `/sessions` | Resume a previous TUI conversation from per-session JSON/JSONL history. |
| `/new` | Start a fresh conversation. |
| `/compact` | Summarize the current transcript and prune older provider context from future turns. |
| `/init` | Create a starter `AGENTS.md` in the active workdir without overwriting an existing file. |
| `/agent`, `/agents` | Open the built-in agent profile selector. |
| `/tools`, `/mcp` | Show builtin tool availability and current MCP status. |
| `/export` | Export the current transcript as Markdown under `YACA_EXPORT_DIR` or `~/.yaca/exports`. |
| `/quit`, `/exit` | Exit the TUI. |
| `/help` | Show available commands and shortcuts. |

Project and user custom commands are loaded from `.opencode/commands/*.md`,
`.opencode/command/*.md`, `~/.config/opencode/commands/*.md`, and
`~/.config/opencode/command/*.md`. Frontmatter fields `description`, `agent`,
and `model` are applied when the command is submitted; command bodies can use
`$ARGUMENTS` and `$1`...`$9`.

`@` file and directory mentions are expanded before a TUI prompt is sent. For
example, `review @src/lib.rs#L10-20` keeps the visible prompt and appends a
bounded context block containing the requested file lines. Directory mentions
append a short listing. A leading agent mention such as `@plan sketch this`
switches to that built-in profile for the submitted turn.

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
- **Split TUI history.** Interactive history is mirrored to independent
  session bundles under `YACA_HISTORY_DIR` or `~/.yaca/history`, with
  `meta.json` plus `events.jsonl` per session and a rebuildable `index.json`.
- **Normalized providers.** Provider-specific protocols decode into one `Event`
  stream; a router resolves a `ModelRef` to a provider and runs capability
  preflight before streaming.
- **Engine-owned stop authority.** In goal mode a separate evaluator judges only
  the transcript; in loop mode a verifier (not the planner) is the sole judge of
  success, behind explicit iteration/no-progress caps.
- **Isolation.** Team members run in their own sessions with scoped permissions
  and per-agent worktrees.

## Configuration

yaca reads its own config at `~/.config/yaca/config.yaml` (honoring
`$XDG_CONFIG_HOME`). Each entry under `providers` builds a real HTTP route:

```yaml
default_model: claude-sonnet-4-6        # optional; else a sonnet model, then the first
providers:
  my-gateway:
    kind: openai                        # openai | anthropic | google
    base_url: https://gateway.example/v1
    api_key: "{env:MY_API_KEY}"         # optional: literal | {env:VAR} | {file:path}
    models: [claude-sonnet-4-6, gpt-5.5]
```

- `kind: openai` → OpenAI Chat Completions (`Authorization: Bearer`)
- `kind: anthropic` → Anthropic Messages (`x-api-key` + `anthropic-version`)
- `kind: google` → Gemini
- `base_url` + the `models` list define the route and which model ids it serves.

API keys resolve from `~/.config/yaca/auth/<id>.yaml` (saved via `yaca login <id>
<token>`) first, then the provider's inline `api_key`. Keys are held in
`SecretString`, sent as sensitive headers, and never logged. Redirects are
disabled so an auth header can't follow a 3xx to another host. Responses stream
over SSE.

Select the model with `--model <id>` or `YACA_MODEL`; otherwise `default_model`,
then a `sonnet` model, then the first available. With no usable config, yaca falls
back to an offline echo provider so the stack still runs.

## Development

The per-change quality gate:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
