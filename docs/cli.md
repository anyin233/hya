# CLI Reference

The backend CLI/API binary is `hya-backend`, defined in
[`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs).

## Global Options

```text
hya-backend [--model <MODEL>] [--prompt <GOAL>] [--max-iterations <N>]
     [--yolo] [--db <PATH>] [--resume <SESSION>] [--mini] [COMMAND]
```

| Option | Meaning |
| --- | --- |
| `--model <MODEL>` | Override `default_model` from hya config and `HYA_MODEL`. |
| `-p, --prompt <GOAL>` | Run headless goal mode instead of the TUI or a subcommand. |
| `--max-iterations <N>` | Iteration cap for goal mode. Defaults to `6` in the CLI. |
| `--yolo` | Auto-approve every tool action. This applies to TUI, headless, and server composition. |
| `--db <PATH>` | SQLite database for the interactive TUI. Empty string uses an in-memory store. |
| `--resume <SESSION>` | Resume a session in the interactive TUI. Raw UUID and `ses_...` ids are accepted. |
| `--mini` | OpenCode-compatible alias for the default TUI. Must be used without a subcommand. |
| `--print-logs`, `--log-level`, `--pure` | Accepted OpenCode-compatible global flags. |

When `--prompt` is present, it takes precedence over subcommand dispatch.

## `hya` frontend

```sh
hya
```

Starts the interactive terminal UI. If stdout is not a terminal, hya prints a
short help message and exits successfully.

The TUI uses the same `SessionEngine` as the rest of the binary. It uses an
in-memory store unless `--db <PATH>` is supplied. Read-only tools are
auto-allowed; mutating tools ask through the permission panel unless `--yolo` is
set. In the `hya` frontend, use the command palette's **Switch YOLO** action to
toggle auto-approval interactively.

TUI slash commands include:

| Command | Meaning |
| --- | --- |
| `/model`, `/models` | Open the model selector. |
| `/resume`, `/sessions` | Resume a prior JSONL-backed TUI session. |
| `/new`, `/clear` | Start a fresh session. |
| `/compact` | Compact older transcript context for future provider requests. |
| `/init` | Create a starter `AGENTS.md` if one does not already exist. |
| `/agent`, `/agents` | Select a built-in agent profile. |
| `/tools`, `/mcp` | Show builtin tools and MCP status. |
| `/think` | Set reasoning effort for future turns. |
| `/export` | Write the current transcript as Markdown. |
| `/quit`, `/exit`, `/q` | Exit the TUI. |
| `/help`, `/?` | Show command help. |

Custom markdown commands are loaded from opencode-style command directories and
hya prompt directories in the project and user config:

```text
~/.config/opencode/commands/*.md
~/.config/opencode/command/*.md
~/.config/hya/prompts/*.md
<workdir>/.opencode/commands/*.md
<workdir>/.opencode/command/*.md
<workdir>/.hya/prompts/*.md
```

Their bodies support `$ARGUMENTS` and positional `$1`...`$9` replacement;
optional `description`, `agent`, and `model` frontmatter is applied when the
command is submitted.

`@path` mentions in TUI prompts are expanded into bounded context blocks before
submission. `@file#Lx-y` includes only the requested line range; `@directory`
includes a short listing. A leading built-in agent mention, for example
`@explore trace this path`, switches that submitted turn to the matching
profile.

## `hya-backend exec`

```sh
hya-backend exec "summarize this repo"
hya-backend exec --json "summarize this repo"
```

Runs one headless turn and prints the rendered transcript. The command uses an
in-memory store, so it does not persist the session. `--json` prints the
canonical event stream as JSONL.

## `hya-backend run`

```sh
hya-backend run "summarize this repo"
hya-backend run --format json "summarize this repo"
```

OpenCode-compatible alias for `exec`. Message words are joined with spaces.
`--format json` and `--json` both emit event JSONL.

## `hya-backend -p`

```sh
hya-backend -p "make the workspace compile" --max-iterations 6
```

Runs goal mode. Each iteration runs an agent turn, then an independent evaluator
judges the transcript. The run stops when the evaluator returns `met=true`, a
cap is reached, or cancellation is requested.

## `hya-backend serve`

```sh
hya-backend serve --bind 127.0.0.1:8080 --db hya.db
```

Starts the HTTP/SSE API from [`../crates/hya-server`](../crates/hya-server).

| Flag | Meaning |
| --- | --- |
| `--bind <ADDR>` | Socket address. Defaults to `127.0.0.1:8080`; use `127.0.0.1:0` for an ephemeral port. |
| `--hostname <HOST>` | OpenCode-compatible alias for the host part of `--bind`. |
| `--port <PORT>` | OpenCode-compatible alias for the port part of `--bind`. |
| `--mdns` | Bind to `0.0.0.0` when no hostname is supplied. hya does not advertise mDNS yet. |
| `--mdns-domain <NAME>` | Accepted for OpenCode CLI compatibility. |
| `--cors <ORIGIN>` | Accepted for OpenCode CLI compatibility; hya mirrors CORS origins globally. |
| `--db <PATH>` | SQLite path. Empty string uses an in-memory store. |

The server mounts native `/sessions/*` routes plus OpenCode-compatible legacy
and v2 route groups. See
[`opencode-parity.md`](opencode-parity.md) for exact compatibility status.

## Auth and Catalog Commands

```sh
hya-backend login <provider> <token>
hya-backend auth list
hya-backend auth logout <provider>
hya-backend providers list
hya-backend providers logout <provider>
hya-backend models [provider] [--verbose] [--refresh]
hya-backend agent list
```

`login` writes a provider token under `~/.config/hya/auth`; saved tokens take
precedence over inline `api_key` values. `providers` is an alias for `auth`.
`models --refresh` is accepted for OpenCode compatibility but does not fetch a
remote catalog.

## Session and RPC Commands

```sh
hya-backend sessions --db hya.db
hya-backend rpc
```

`sessions` lists persisted sessions in a SQLite database. `rpc` reads JSONL
requests on stdin, accepts `{"type":"prompt","text":"..."}` and
`{"type":"quit"}`, and emits new session events plus a `{"type":"done"}` marker.

## `hya-backend tail-session`

```sh
hya-backend tail-session <session-uuid> --db hya.db
```

Replays a persisted session's event log as JSON lines. The `<session-uuid>` is
the raw UUID portion, not the display form with the `ses_` prefix.

This command intentionally exits cleanly on broken pipe, so shell filters such
as `head` and `grep -q` can close stdout without causing a panic.
