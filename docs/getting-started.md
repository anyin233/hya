# Getting Started

This guide runs hya from the workspace. The frontend TUI binary is `hya`; the
backend CLI/API binary is `hya-backend`.

## Prerequisites

- Rust toolchain compatible with the workspace manifest in [`../Cargo.toml`](../Cargo.toml).
- A terminal that supports alternate-screen TUI programs.
- Optional: a hya provider config if you want live model calls. Without
  one, hya uses an offline development provider that echoes prompts.

## Build

```sh
cargo build --workspace
```

Building does not create `~/.config/hya`; the starter config is created on the
first `hya` or `hya-backend` startup that needs runtime config.

Install the frontend from [`../crates/hya`](../crates/hya) and the backend CLI
from [`../crates/hya-backend`](../crates/hya-backend):

```sh
cargo install --path crates/hya
cargo install --path crates/hya-backend
```

## Run the TUI

```sh
cargo run -p hya --
```

or, after installing:

```sh
hya
```

The TUI creates an in-memory session, streams assistant events into the chat
view, and prompts for permission when a tool requests a mutating action.

Key controls:

| Key | Action |
| --- | --- |
| `Enter` | Send the current input when no turn is running. |
| `PgUp` / `PgDn` | Scroll the conversation. |
| `Up` / `Down` | Scroll one line. |
| `Tab` on `/` input | Complete slash commands or open the command picker. |
| `F2` | Open the model selector. |
| `Ctrl-P` | Open command/help. |
| `Ctrl-C` | Close dialogs, clear input, interrupt a running turn, or exit when idle. |

## Run One Headless Turn

```sh
cargo run -p hya-backend -- exec "summarize this repository"
```

`exec` creates a session using the global `--db <PATH>` SQLite store when
supplied (otherwise in-memory), admits one user prompt, runs one assistant turn,
and prints the transcript. With `--db`, hya stores the full canonical event log,
which can include prompts, tool arguments, tool results, reasoning deltas,
command metadata, and absolute workdir paths. Add `--json` to emit canonical
event JSONL.

OpenCode-compatible prompt execution is also accepted:

```sh
cargo run -p hya-backend -- run --format json "summarize this repository"
```

To persist a headless session for replay, put `--db` before the subcommand:

```sh
cargo run -p hya-backend -- --db ./hya.db exec "summarize this repository"
```

Use a private path for persisted databases. They are plain SQLite files; hya does
not encrypt them or override the process umask.

## Run Goal Mode

```sh
cargo run -p hya-backend -- -p "make all tests pass" --max-iterations 6
```

Goal mode iterates with an in-memory store until an independent evaluator says
the goal is met or a cap is reached. It is driven by `run_goal` in
[`../crates/hya-core/src/completion.rs`](../crates/hya-core/src/completion.rs)
and does not persist to the global `--db` database.

## Run the HTTP/SSE Server

```sh
cargo run -p hya-backend -- serve --bind 127.0.0.1:8080 --db hya.db
```

Use an empty `--db ""` for an in-memory store, or a file path for SQLite
persistence.

The server prints the address it bound to:

```text
hya server listening on http://127.0.0.1:8080
```

The same server exposes native `/sessions/*` routes and OpenCode-compatible
legacy/v2 route groups for sessions, events, files, providers/models,
permissions/questions, MCP, PTY, VCS, projects/worktrees, TUI control, and sync.

## Replay a Session

```sh
cargo run -p hya-backend -- tail-session <session-id> --db hya.db
```

`tail-session` reads the persisted event log and prints one JSON `Envelope` per
line. The `<session-id>` can be a `hysec_...` id from `sessions --db`, a legacy
`ses_...` display id, or a legacy raw UUID. It is useful for debugging because it
shows the same canonical events that the server streams over SSE.

## From Offline to a Live Provider

Out of the box hya runs **offline**: with no config it uses a development
provider that echoes your prompt. You can tell you are offline because the model
id shows as `offline` and replies are prefixed `(hya dev provider)`. This is
intentional, not an error — see
[Configuration → First-Run / Offline Behavior](configuration.md#first-run--offline-behavior).

hya creates a starter `~/.config/hya/config.yaml` (or
`$XDG_CONFIG_HOME/hya/config.yaml`) the first time a command needs runtime
config. Interactive startup also offers to import provider/model entries from
your OpenCode config. You can run the same model-only import explicitly:

```sh
hya --import opencode
```

To switch to a live model manually, edit the starter file:

```yaml
default_model: claude-sonnet-4-6
providers:
  anthropic:
    kind: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: "{env:ANTHROPIC_API_KEY}"
    models: [claude-sonnet-4-6]
```

Then provide the key and confirm the catalog resolved:

```sh
export ANTHROPIC_API_KEY=sk-...                # or use `hya-backend login` instead of {env:...}
hya-backend login anthropic "$ANTHROPIC_API_KEY"   # optional; takes precedence over api_key
hya-backend models                            # should list claude-sonnet-4-6, not be empty
hya                                    # TUI now runs against the live provider
```

`hya-backend login <provider> <token>` stores an auth token that takes precedence over
inline `api_key`. For a fully-commented sample config, the complete `HYA_*`
environment-variable reference, and MCP/plugin setup, see
[Configuration](configuration.md). For the full command and TUI slash-command
reference, see the [CLI Reference](cli.md).
