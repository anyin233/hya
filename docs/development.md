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
cargo test --workspace
```

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

### TUI / SDK real-backend (Track T)

```sh
cargo build -p hya-backend --bin hya-backend
cd packages/hya-tui-ts
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

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
| TUI behavior | [TUI](architecture/tui.md) |
| Agent process E2E / matrix | [Testing](testing/README.md), [Agent matrix](testing/agent-matrix.md) |

Keep docs grounded in shipped behavior. If a table or schema reserves space for
future functionality that is not wired into the current read path, say that
plainly.
