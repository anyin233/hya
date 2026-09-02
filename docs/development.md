# Development

This page covers the Rust workspace itself: build, formatting, linting, tests,
and how to choose the right crate for a change.

## Workspace

The workspace root is [`../Cargo.toml`](../Cargo.toml). It uses:

- Rust edition `2024`
- resolver `3`
- Rust version `1.91`
- shared workspace dependency versions
- workspace clippy lints denying `unwrap_used` and `expect_used`

Library code should return typed errors instead of panicking. Binaries and tests
may use local allowances when appropriate.

## Build and Quality Gate

Run the standard gate before publishing code changes:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
```

`--exclude hya-e2e` matches CI and [Testing](testing/README.md): Track P spawns
real backend processes and must not run multi-threaded under the default suite.
CI also uses `--jobs 1` on that step; local runs may omit the job cap. Run
process E2E separately (below).

For docs-only changes, at least run a local Markdown link check and a scan for
accidental references to repository-private process notes that do not belong in
project docs.

### Process agent E2E (Track P)

Product-path coverage lives in `crates/hya-e2e` (real `hya-backend` + FakeLlm).
It needs a built backend binary and should run single-threaded:

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
cargo clippy -p hya-e2e --all-targets -- -D warnings
```

See [Testing](testing/README.md), [Process E2E](testing/process-e2e.md), and the
[agent feature matrix](testing/agent-matrix.md). Optional CI wiring is sketched
in [ci-agent-e2e-snippet.yml](testing/ci-agent-e2e-snippet.yml).

### TypeScript frontend (Track T)

From `packages/hya-tui-ts`:

| Command | What it does |
| --- | --- |
| `bun run build` | `bun build src/main.tsx --outdir dist --target bun --packages external` — emits under `dist/` (Bun resolves `with { type: "file" }` audio imports used by attention sounds). |
| `bun run typecheck` | `tsgo --noEmit` over `src` and `test` (`jsx: preserve`, `jsxImportSource: @opentui/solid`). |
| `bun test` | Full package test suite (see suites below). |

**Preload prerequisite.** Both the runtime and the test runner must preload
`@opentui/solid/preload` via [`bunfig.toml`](../packages/hya-tui-ts/bunfig.toml).
Without it, TUI JSX does not resolve — check this first when a fresh checkout
fails to render or tests fail to compile.

**Test suites** under `packages/hya-tui-ts/test/`:

The package currently contains 14 Bun test files. The Workflow suites are
`workflow-presentation`, `workflow-sidebar`, and `workflow-pty`; the first two
are focused presentation tests and the PTY suite requires built backend and TUI
binaries. The full package suite remains the source of truth for the complete
file list.

| Suite | Role |
| --- | --- |
| `boundary` | Forbidden imports/paths and pinned dependency versions |
| `branding-pruning` | Excluded upstream surface and branding stability |
| `sdk-spine` | Headless SDK/sync provider chain |
| `runtime-boundary` | Staged runtime install + prune + build probe |
| `startup-trace` | Startup mark emission |
| `agent-visibility` | Agent picker / `@` autocomplete rules |
| `task-presentation` | Multi-member task row presentation |
| `subagent-workspace` | Pane reducer / run tree |
| `workflow-presentation` | Typed Workflow projection and presentation |
| `workflow-sidebar` | Workflow sidebar plugin registration and rendering |
| `workflow-pty` | Workflow presentation over a real backend PTY |
| `pty-smoke` | End-to-end PTY smoke against a real backend |
| `real-backend` | Real-backend permission/question flows |
| `real-backend-agents` | Real-backend multi-agent roster |

Focused real-backend runs (after `cargo build -p hya-backend --bin hya-backend`):

```sh
cd packages/hya-tui-ts
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

**Scripts:**

- `scripts/prune-sdk-server.ts` — post-install step that rewrites the installed
  `@opencode-ai/sdk` export map down to the v2 client, deletes server/process
  bundles, and probes that `createOpencodeClient` still imports. Re-run after any
  SDK dependency bump (the installer runs it automatically).
- `scripts/generate-logo-art.py` — regenerates `component/logo-art.data.ts` and
  `util/epilogue-art.data.ts` from the 8-bit Hya wordmark PNG. Only needed when
  the wordmark asset changes (see script header for `uv run` invocation).

## Dev tasks (`xtask` package)

`crates/xtask` is **dev-only tooling** and is not part of any shipped binary.
There is **no** Cargo alias named `xtask` in this workspace: invoke it as
`cargo run -p xtask -- <task> …`. The binary uses a hand-rolled positional
dispatcher (not clap): the first positional argument selects the task and every
remaining argument is forwarded verbatim. The currently supported tasks are
`sync-compat`, `migrate`, `startup-bench`, `matrix-check`, `package-bundle`, and
`release-rehearsal`.

| Task | Role |
| --- | --- |
| `sync-compat` | Import supported MCP servers and skills from an OpenCode/Compat config into hya config / skill roots. Does not import providers or models — use `hya --import compat` for those. |
| `migrate` | Alias that dispatches to the same implementation as `sync-compat`. |
| `startup-bench` | Startup latency benchmark. Honours `HYA_BACKEND_BIN` to select the binary under test. |
| `matrix-check` | Validates `crates/hya-e2e/matrix.toml`. See [agent-matrix.md](testing/agent-matrix.md). |
| `package-bundle` | Validates a source directory and atomically writes the canonical deterministic public `.hyabundle` package. |
| `release-rehearsal` | Runs the pinned, non-publishing release build/package/smoke rehearsal, including archive, adapter, Argus, and runtime-prune checks. |

```sh
cargo run -p xtask -- sync-compat --help   # args after the task name are forwarded
cargo run -p xtask -- matrix-check
cargo run -p xtask -- startup-bench
cargo run -p xtask -- package-bundle <source-dir> <output.hyabundle>
cargo run -p xtask -- release-rehearsal --workflow .github/workflows/release.yml --version 0.36.9 --target x86_64-unknown-linux-gnu --no-publish
```

### Example-only environment

`HYA_BACKEND_DIR` is read only by the SDK native-bridge example
([`crates/hya-sdk/examples/native_spike.rs`](../crates/hya-sdk/examples/native_spike.rs))
and names the package directory for that bridge. It has **no** effect on `hya`,
`hya-backend`, or the TUI — do not treat it as user configuration.

## Crate Selection

Use this guide when deciding where a change belongs:

| Change | Crate |
| --- | --- |
| New event, id, API DTO, message field, projection behavior | `hya-proto` |
| New provider route, protocol encoder/decoder, capability preflight | `hya-provider` |
| New builtin tool or permission action | `hya-tool` |
| Persistence, replay, migrations, usage ledger | `hya-store` |
| Turn-loop behavior, goal/loop/team/worktree runtime logic | `hya-core` |
| HTTP route or SSE behavior | `hya-server` |
| Typed HTTP integration | `hya-client` |
| Terminal UI rendering and interaction | `packages/hya-tui-ts` |
| Frontend entrypoint and process supervision | `hya`, `hya-ts` |
| User-facing backend CLI command, config loading, server launch | `hya-backend` |
| Process-level agent scenario (real backend + FakeLlm) | `hya-e2e` (+ matrix docs under `docs/testing/`) |
| Dev tooling (`sync-compat`, matrix check, startup bench) | `xtask` |

## Testing Strategy

Prefer crate-local tests that assert boundary behavior:

- Provider tests should compare canonical event shape, not just provider JSON.
- Store tests should replay and fold projections.
- Core tests should exercise turn loops and stop conditions with fake providers.
- Tool tests should cover permission behavior and output limits.
- TUI tests should render states without requiring a live terminal.
- Server tests should verify route behavior through the Axum router.

Layer product paths on top of crate-local suites:

| Track | Home | Role |
| --- | --- | --- |
| I (in-process) | Each crate's `tests/` | Deep engine/API contracts (index authority for nested spawn, resident, etc.) |
| P (process) | `crates/hya-e2e` | Real binary + FakeLlm: tools, permissions, skills, MCP, subagents, hyabundle |
| T (TUI/SDK) | `packages/hya-tui-ts/test` | Real-backend permission/question, roster, multi-agent presentation |

Do not weaken Track P oracles to request counts or tool-call argument substrings
alone — require disk effects, tree depth, follow-up FakeLlm tool **results**, or
API listing of package agents as documented in [process-e2e.md](testing/process-e2e.md).

## Documentation Updates

When changing a boundary, update the nearest docs page:

| Boundary | Docs page |
| --- | --- |
| CLI behavior | [CLI Reference](cli.md) |
| Config behavior | [Configuration](configuration.md) |
| Crate/file layout | [Project Structure](project-structure.md) |
| Runtime behavior | [Runtime](architecture/runtime.md) |
| Events/projection | [Event Model](architecture/event-model.md) |
| Providers | [Providers](architecture/providers.md) |
| Tools/permissions | [Tools and Permissions](architecture/tools-and-permissions.md) |
| Store/schema | [Storage](architecture/storage.md) |
| Server/client API | [Server and Client](architecture/server-client.md) |
| TUI behavior | [TUI](architecture/tui.md), [TUI Reference](tui-reference.md), [TUI Keybindings](tui-keybindings.md) |
| Agent process E2E / matrix | [Testing](testing/README.md), [Agent matrix](testing/agent-matrix.md) |

Keep docs grounded in shipped behavior. If a table or schema reserves space for
future functionality that is not wired into the current read path, say that
plainly.
