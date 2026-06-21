# Getting Started

This guide runs yaca from the workspace. The same commands apply to an installed
`yaca` binary once you install `crates/yaca-cli`.

## Prerequisites

- Rust toolchain compatible with the workspace manifest in [`../Cargo.toml`](../Cargo.toml).
- A terminal that supports alternate-screen TUI programs.
- Optional: an opencode provider config if you want live model calls. Without
  one, yaca uses an offline development provider that echoes prompts.

## Build

```sh
cargo build --workspace
```

The main binary lives in [`../crates/yaca-cli`](../crates/yaca-cli). To install it
from this checkout:

```sh
cargo install --path crates/yaca-cli
```

## Run the TUI

```sh
cargo run -p yaca-cli --
```

or, after installing:

```sh
yaca
```

The TUI creates an in-memory session, streams assistant events into the chat
view, and prompts for permission when a tool requests a mutating action.

Key controls:

| Key | Action |
| --- | --- |
| `Enter` | Send the current input when no turn is running. |
| `PgUp` / `PgDn` | Scroll the conversation. |
| `Up` / `Down` | Scroll one line. |
| `Esc`, `Ctrl-C`, `Ctrl-D` | Quit. |

## Run One Headless Turn

```sh
cargo run -p yaca-cli -- exec "summarize this repository"
```

`exec` creates an in-memory session, admits one user prompt, runs one assistant
turn, and prints the transcript.

## Run Goal Mode

```sh
cargo run -p yaca-cli -- -p "make all tests pass" --max-iterations 6
```

Goal mode iterates until an independent evaluator says the goal is met or a cap
is reached. It is driven by `run_goal` in
[`../crates/yaca-core/src/completion.rs`](../crates/yaca-core/src/completion.rs).

## Run the HTTP/SSE Server

```sh
cargo run -p yaca-cli -- serve --bind 127.0.0.1:8080 --db yaca.db
```

Use an empty `--db ""` for an in-memory store, or a file path for SQLite
persistence.

The server prints the address it bound to:

```text
yaca server listening on http://127.0.0.1:8080
```

## Replay a Session

```sh
cargo run -p yaca-cli -- tail-session <session-uuid> --db yaca.db
```

`tail-session` reads the persisted event log and prints one JSON `Envelope` per
line. It is useful for debugging because it shows the same canonical events that
the server streams over SSE.
