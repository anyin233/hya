# Getting Started

This guide runs hya from the workspace. The only shipped binary is the backend
CLI/API binary `hya`. The interactive OpenTUI frontend (`packages/hya-tui`)
runs from source with Bun against `hya serve`; see [OpenTUI frontend](tui.md).
Other clients drive the backend over the `hya.v1` HTTP/SSE/WebSocket or gRPC
contract.

## Prerequisites

- Rust 1.91 or later.
- Bun 1.3.x (used by the Compat plugin sidecar).
- Git.
- Optional: a hya provider config if you want live model calls. Without
  one, hya uses an offline development provider that echoes prompts.

## Build

```sh
cargo build --workspace
```

Building does not create `~/.config/hya`; the starter config is created on the
first `hya` startup that needs runtime config.

### Install from source (`./install.sh`)

Build and install the backend runtime layout:

```sh
./install.sh --prefix "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

#### Options

| Option | Meaning |
| --- | --- |
| `--prefix DIR` | Install into `DIR/bin`, `DIR/bundles`, and `DIR/lib/hya` (default `/usr/local`). |
| `--bin-dir DIR` | Install the backend into `DIR`, which must be named `bin` (overrides `--prefix`). The backend loads its first-party bundles from `DIR/../bundles`, so any other name exits 2. Relative paths resolve against the script directory. |
| `--profile release\|dev\|debug` | Cargo build profile and matching target dir (honours `CARGO_TARGET_DIR`). Any other value exits 2. |
| `--dry-run` | Print every action; skip building and installing; print verification commands instead of running them. |
| `-h` / `--help` | Print usage and exit 0. |

#### What the installer does

Failures are easiest to diagnose if you know the order of operations
([`install.sh`](../install.sh)):

1. **Permission preflight.** Walks up to the nearest existing ancestor of the
   target `bin`, `bundles`, and `lib` directories. If that ancestor is not a
   writable directory, prints remedies (`sudo ./install.sh` or
   `./install.sh --prefix "$HOME/.local"`) and exits 1.
2. **Bun preflight.** `bun --version` must succeed or the install aborts.
3. **Cargo build.** Builds the locked `hya` binaries and the five
   tool-family libraries for the selected profile.
4. **Stage runtimes.** Stages the `hya` binary, packages the twelve
   first-party bundles with `cargo run -p xtask -- stage-first-party-bundles`,
   and stages the Bun adapter at `lib/hya/bun-adapter` with its pinned lockfile
   by running `bun install --frozen-lockfile --production`.
5. **Atomic swap.** Only complete staged artifacts reach the swap. The script
   uses `.tmp.$$` paths and moves any existing backend, adapter, and
   `bundles/hya-*.hyabundle` files to `.bak.$$`, then renames into place. Other
   files in `bundles/` are left alone. An `ERR`/`INT`/`TERM` trap calls
   `restore_install` so a failed or interrupted install restores the previous
   backend, adapter, and bundles and cleans leftovers; it does not leave a
   half-installed `hya`.
6. **Post-install verification** (skipped under `--dry-run`, which only
   prints the checks):
   - Runs `hya --version` and `hya --help`.
   - Runs `hya bundle list` with an isolated `HOME` and requires every
     first-party bundle, which proves the installed backend loads them.
   - Asserts the Bun adapter payload and its production dependencies exist
     under `lib/hya/bun-adapter`.
   - **Fails** if `command -v hya` does not resolve to the install path
     (usual cause: an older `hya` earlier on `PATH`).

The installer produces the same layout as a release archive: `bin/hya`,
`bundles/hya-*.hyabundle`, and `lib/hya/bun-adapter`. Bare `hya` (no subcommand)
prints a guidance banner; see the
[CLI Reference](cli.md#bare-hya).

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

The server serves the consolidated `hya.v1` contract under `/v1`
(HTTP/JSON + SSE + WebSocket): process/catalog/auth, sessions and event-driven
turns, messages/todo, event replay and streams, unified
permission/question interactions, Workflow, files, project/VCS/worktrees, MCP,
PTY, and logs. Setting `HYA_GRPC_BIND=<host:port>` additionally serves the same
contract over gRPC. See the [Protocol guide](protocol/README.md) and the
generated [API reference](protocol/api-reference.md).

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
config. To switch to a live model, edit the starter file:

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
export ANTHROPIC_API_KEY=sk-...                # or use `hya login` instead of {env:...}
hya login anthropic "$ANTHROPIC_API_KEY"   # optional; takes precedence over api_key
hya models                            # should list claude-sonnet-4-6, not be empty
```

`hya login <provider> <token>` stores an auth token that takes precedence over
inline `api_key`. For a fully-commented sample config, documented environment
variables, and MCP/plugin setup, see [Configuration](configuration.md). Note that
the configuration page lists selected `HYA_*` variables used by common workflows.
For CLI commands, see the [CLI Reference](cli.md). To integrate a client over the
API, see the [Protocol guide](protocol/README.md).
