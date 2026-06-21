# CLI Reference

The shipped binary is `yaca`, defined in
[`../crates/yaca-cli/src/main.rs`](../crates/yaca-cli/src/main.rs).

## Global Options

```text
yaca [--model <MODEL>] [--prompt <GOAL>] [--max-iterations <N>] [COMMAND]
```

| Option | Meaning |
| --- | --- |
| `--model <MODEL>` | Override the opencode default and `YACA_MODEL`. |
| `-p, --prompt <GOAL>` | Run headless goal mode instead of the TUI or a subcommand. |
| `--max-iterations <N>` | Iteration cap for goal mode. Defaults to `6` in the CLI. |

When `--prompt` is present, it takes precedence over subcommand dispatch.

## `yaca`

```sh
yaca
```

Starts the interactive terminal UI. If stdout is not a terminal, yaca prints a
short help message and exits successfully.

The TUI uses an in-memory store and the same `SessionEngine` as the rest of the
binary. Read-only tools are auto-allowed; mutating tools ask through the
permission panel.

TUI slash commands include:

| Command | Meaning |
| --- | --- |
| `/model`, `/models` | Open the model selector. |
| `/resume`, `/sessions` | Resume a prior JSONL-backed TUI session. |
| `/new` | Start a fresh session. |
| `/compact` | Compact older transcript context for future provider requests. |
| `/init` | Create a starter `AGENTS.md` if one does not already exist. |
| `/agent`, `/agents` | Select a built-in agent profile. |
| `/tools`, `/mcp` | Show builtin tools and MCP status. |
| `/export` | Write the current transcript as Markdown. |
| `/quit`, `/exit` | Exit the TUI. |
| `/help`, `/?` | Show command help. |

Custom markdown commands are loaded from opencode-style command directories in
the project and user config. Their bodies support `$ARGUMENTS` and positional
`$1`...`$9` replacement; optional `agent` and `model` frontmatter is applied
when the command is submitted.

`@path` mentions in TUI prompts are expanded into bounded context blocks before
submission. `@file#Lx-y` includes only the requested line range; `@directory`
includes a short listing. A leading built-in agent mention, for example
`@explore trace this path`, switches that submitted turn to the matching
profile.

## `yaca exec`

```sh
yaca exec "summarize this repo"
```

Runs one headless turn and prints the rendered transcript. The command uses an
in-memory store, so it does not persist the session.

## `yaca -p`

```sh
yaca -p "make the workspace compile" --max-iterations 6
```

Runs goal mode. Each iteration runs an agent turn, then an independent evaluator
judges the transcript. The run stops when the evaluator returns `met=true`, a
cap is reached, or cancellation is requested.

## `yaca serve`

```sh
yaca serve --bind 127.0.0.1:8080 --db yaca.db
```

Starts the HTTP/SSE API from [`../crates/yaca-server`](../crates/yaca-server).

| Flag | Meaning |
| --- | --- |
| `--bind <ADDR>` | Socket address. Defaults to `127.0.0.1:8080`; use `127.0.0.1:0` for an ephemeral port. |
| `--db <PATH>` | SQLite path. Empty string uses an in-memory store. |

## `yaca tail-session`

```sh
yaca tail-session <session-uuid> --db yaca.db
```

Replays a persisted session's event log as JSON lines. The `<session-uuid>` is
the raw UUID portion, not the display form with the `ses_` prefix.

This command intentionally exits cleanly on broken pipe, so shell filters such
as `head` and `grep -q` can close stdout without causing a panic.
