# Getting Started

This guide runs hya from the workspace. The frontend TUI binary is `hya`; the
backend CLI/API binary is `hya-backend`.

## Prerequisites

- Rust 1.91 or later.
- Bun 1.3.x.
- Git.
- A terminal that supports alternate-screen TUI programs.
- Optional: a hya provider config if you want live model calls. Without
  one, hya uses an offline development provider that echoes prompts.

## Build

```sh
cargo build --workspace
```

Building does not create `~/.config/hya`; the starter config is created on the
first `hya` or `hya-backend` startup that needs runtime config.

### Install from source (`./install.sh`)

Build and install the complete frontend/runtime layout:

```sh
./install.sh --prefix "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

#### Options

| Option | Meaning |
| --- | --- |
| `--prefix DIR` | Install into `DIR/bin` (default `/usr/local`). |
| `--bin-dir DIR` | Install binaries directly into `DIR` (overrides `--prefix`). Relative paths resolve against the script directory. Runtime assets go under `DIR/../lib/hya/hya-tui-ts`. |
| `--profile release\|dev\|debug` | Cargo build profile and matching target dir (honours `CARGO_TARGET_DIR`). Any other value exits 2. |
| `--dry-run` | Print every action; skip building and installing; print verification commands instead of running them. |
| `-h` / `--help` | Print usage and exit 0. |

#### What the installer does

Failures are easiest to diagnose if you know the order of operations
([`install.sh`](../install.sh)):

1. **Permission preflight.** Walks up to the nearest existing ancestor of the
   target bin and lib directories. If that ancestor is not a writable directory,
   prints remedies (`sudo ./install.sh` or
   `./install.sh --bin-dir "$HOME/.local/bin"`) and exits 1.
2. **Bun preflight.** `bun --version` must succeed or the install aborts.
3. **Cargo build.** Builds locked binaries for `hya`, `hya-backend`, and
   `hya-ts` for the selected profile.
4. **Stage runtimes.** Copies the TUI package into a temporary tree, runs
   `bun install --frozen-lockfile --production`, and prunes the SDK; stages the
   Compat adapter separately at `lib/hya/compat-adapter` with its pinned lockfile.
5. **Atomic swap.** Stages into `.tmp.$$` paths, moves any existing install to
   `.bak.$$`, then renames into place. An `ERR`/`INT`/`TERM` trap calls
   `restore_install` so an interrupted install puts previous binaries and
   runtimes back and cleans leftovers — it should never leave a half-installed
   `hya`.
6. **Post-install verification** (skipped under `--dry-run`, which only
   prints the checks):
   - Runs the `hya` shim against a dead server with `--bun /bin/true`.
   - Runs `hya --version`, `hya-backend --help`, `hya-ts --help`.
   - Asserts TUI runtime files (`src/main.tsx`, `bunfig.toml`, license files)
     and `node_modules` exist under `lib/hya/hya-tui-ts`.
   - Asserts the Compat adapter payload and its production dependencies exist
     under `lib/hya/compat-adapter`.
   - **Fails** if `command -v hya` does not resolve to the install path (usual
     cause: an older `hya` earlier on `PATH`).

The installer colocates `hya`, `hya-ts`, and `hya-backend`, prepares the TUI
runtime under `lib/hya/hya-tui-ts`, and prepares the Compat adapter under
`lib/hya/compat-adapter`. Installing only the `hya` Cargo package is unsupported
because that executable delegates to the adjacent launcher and runtime.

## Run the TUI

```sh
# Installed layout
hya .

# Uninstalled checkout (after `cargo build --workspace`)
./target/debug/hya .
```

`hya` delegates to the TypeScript/OpenTUI frontend. The launcher starts an owned
local `hya-backend`, or attaches to an existing server when `--server <URL>` is
provided. It streams assistant events into the chat view and prompts for
permission when a tool requests a mutating action.

Key controls (defaults; leader is `Ctrl-X`):

| Key | Action |
| --- | --- |
| `Enter` | Send the current input when no turn is running. |
| `Ctrl-P` | List available commands (command palette). |
| `Ctrl-X` | Leader key — arms a timed chord for `<leader>…` bindings. |
| `Escape` | Dismiss a dialog, hide autocomplete, clear a pending leader sequence, exit shell mode, return an observation pane to Main, or interrupt the running turn — press **twice** within 5 s (while the prompt is focused) to abort. |
| `Ctrl-C` | Copy the selection if one is active (when explicit copy is required), clear the prompt if it has text, otherwise exit when the prompt is unfocused or empty. |
| `Ctrl-D` | Exit when the prompt is unfocused **or** empty; deletes forward inside the prompt; deletes the highlighted entry in the Sessions and Stash dialogs. |
| `<leader>l` | List sessions. |
| `<leader>m` | List models. |
| `<leader>a` | List agents. |
| `<leader>o` | Open the subagent roster. |
| `<leader>b` | Toggle the sidebar. |

For the full keybinding tables and built-in slash commands (`/sessions`,
`/models`, `/agents`, `/export`, `/compact`, and the rest), see
[TUI Keybindings](tui-keybindings.md). Screens and dialogs are covered in
[TUI Reference](tui-reference.md).

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

Compat-compatible prompt execution is also accepted:

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

The same server exposes native `/sessions/*` routes and Compat-compatible
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

Out of the box Hya runs **offline**: with no live catalog rows it uses the local
echo provider. The model is `hya/offline`; each reply echoes the prompt and says
that no live provider is available and one must be configured. This is
intentional, not an error — see
[Configuration → First-Run / Offline Behavior](configuration.md#first-run--offline-behavior).

hya creates a starter `~/.config/hya/config.yaml` (or
`$XDG_CONFIG_HOME/hya/config.yaml`) the first time a command needs runtime
config. Canonical `hya` imports Compat configuration only when requested
explicitly:

```sh
hya --import compat
```

This imports providers, models, and supported local MCP servers. Skills import
is not implemented yet. Bare interactive `hya-backend` retains its first-run
import prompt when it creates the starter config.

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
inline `api_key`. For a fully-commented sample config, documented environment
variables, and MCP/plugin setup, see [Configuration](configuration.md). Note that
the configuration page lists selected `HYA_*` variables used by common workflows;
additional process-local flags (for example TUI startup and automation hooks)
appear in [Troubleshooting](troubleshooting.md) and
[TUI Architecture](architecture/tui.md). For CLI commands, see the
[CLI Reference](cli.md). For TUI slash commands and keybindings, see
[TUI Keybindings](tui-keybindings.md).
