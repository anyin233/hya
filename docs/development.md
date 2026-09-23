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

## Task Management

For multi-step work, use planning-with-files under
`.planning/<YYYY-MM-DD-slug>/`:

- `task_plan.md` records phases and decisions; `findings.md` records discoveries;
  `progress.md` records updates and handoffs.
- `.planning/.active_plan` is an optional pointer to the current plan when several
  plan directories coexist.
- Update the plan after each phase and when resuming work after a pause or context
  reset. Small tasks may use a lightweight plan, or no plan when no durable
  context is needed.
- This workflow has no Trellis runtime dependency. `docs/development-history/tasks/`
  and `docs/development-history/workspace/` retain historical task artifacts and
  journals as evidence; they are not live workflow instructions.

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

Product-path coverage lives in `crates/hya-e2e` (real `hya` + FakeLlm).
It needs a built backend binary and should run single-threaded:

```sh
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e -- --test-threads=1
cargo clippy -p hya-e2e --all-targets -- -D warnings
```

See [Testing](testing/README.md), [Process E2E](testing/process-e2e.md), and the
[agent feature matrix](testing/agent-matrix.md). Optional CI wiring is sketched
in [ci-agent-e2e-snippet.yml](testing/ci-agent-e2e-snippet.yml).

There is currently no interactive TUI (the legacy TypeScript TUI was removed;
a replacement built on `hya-sdk-v1` may be built later), so no frontend
TypeScript gate exists.

## Dev tasks (`xtask` package)

`crates/xtask` is **dev-only tooling** and is not part of any shipped binary.
There is **no** Cargo alias named `xtask` in this workspace: invoke it as
`cargo run -p xtask -- <task> …`. The binary uses a hand-rolled positional
dispatcher (not clap): the first positional argument selects the task and every
remaining argument is forwarded verbatim. The currently supported tasks are
`startup-bench`, `matrix-check`, `package-bundle`, `package-native-tool-bundle`,
`package-native-tool-library`, `stage-first-party-bundles`, `gen-api`, and
`release-rehearsal`.

| Task | Role |
| --- | --- |
| `gen-api` | Regenerate the `hya.v1` contract from `proto/hya/v1`: Rust types (prost/tonic/pbjson), the API reference, and OpenAPI. Uses a vendored protoc; output is committed, and the task fails when any rpc lacks its `// hya.http:` mapping or two rpcs collide. |
| `startup-bench` | Startup latency benchmark. Honours `HYA_BACKEND_BIN` to select the binary under test. |
| `matrix-check` | Validates `crates/hya-e2e/matrix.toml`. See [agent-matrix.md](testing/agent-matrix.md). |
| `package-bundle` | Validates a source directory and atomically writes the canonical deterministic public `.hyabundle` package. |
| `package-native-tool-bundle` | Adds a built target-specific Rust executable and exact policy tool declarations to one tool-family source, then writes a deterministic public package. |
| `package-native-tool-library` | Adds a built tool-family dynamic library and exact policy tool declarations to one tool-family source, then writes a deterministic public package. |
| `stage-first-party-bundles` | Packages the twelve trusted first-party bundles into `<package-root>/bundles/` and fails if any bundle version differs from the release version. With `--target` and `--assets`, it also writes each package as a versioned standalone release asset. The release workflow, the rehearsal, and `install.sh` all use it. |
| `release-rehearsal` | Runs the pinned, non-publishing release build/package/smoke rehearsal for one target of the release matrix, including archive, first-party bundle assets, checksums, adapter, and Argus checks. Run it on a host of that target (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, or `aarch64-apple-darwin`); it needs `actionlint` 1.7.12, Bun 1.4.2, 7-Zip (`7z`), and `shasum` on `PATH`. |

```sh
cargo run -p xtask -- matrix-check
cargo run -p xtask -- startup-bench
cargo run -p xtask -- package-bundle <source-dir> <output.hyabundle>
cargo run -p xtask -- package-native-tool-bundle <tool-family-source-dir> <built-executable> <output.hyabundle>
cargo run -p xtask -- package-native-tool-library <tool-family-source-dir> <built-library> <output.hyabundle>
cargo run -p xtask -- stage-first-party-bundles --library-dir target/release --package-root dist/hya [--version <semver>] [--target <triple> --assets dist]
cargo run -p xtask -- release-rehearsal --workflow .github/workflows/release.yml --version 0.41.0 --target "$(rustc -vV | sed -n 's/^host: //p')" --no-publish
```

## Crate Selection

Use this guide when deciding where a change belongs:

| Change | Crate |
| --- | --- |
| New event, id, API DTO, message field, projection behavior | `hya-proto` |
| New provider route, protocol encoder/decoder, capability preflight | `hya-provider` |
| New builtin tool implementation | Owning source under `bundles/presets/<family>-tools/native` |
| Tool trait, loader, runtime plane, or permission action | `hya-tool` |
| Persistence, replay, migrations, usage ledger | `hya-store` |
| Turn-loop behavior, goal/loop/team/worktree runtime logic | `hya-core` |
| HTTP route or SSE behavior | `hya-server` |
| `hya.v1` contract change (proto message/rpc, error code, HTTP binding) | `hya-api` — edit `proto/hya/v1/*.proto`, then regenerate with `cargo run -p xtask -- gen-api` |
| Typed HTTP integration | `hya-client`; new frontend integrations use `hya-sdk-v1` |
| User-facing backend CLI command, config loading, server launch | `hya` |
| Process-level agent scenario (real backend + FakeLlm) | `hya-e2e` (+ matrix docs under `docs/testing/`) |
| Dev tooling (matrix check, startup bench) | `xtask` |

## Testing Strategy

Prefer crate-local tests that assert boundary behavior:

- Provider tests should compare canonical event shape, not just provider JSON.
- Store tests should replay and fold projections.
- Core tests should exercise turn loops and stop conditions with fake providers.
- Tool tests should cover permission behavior and output limits.
- Server tests should verify route behavior through the Axum router.

Layer product paths on top of crate-local suites:

| Track | Home | Role |
| --- | --- | --- |
| I (in-process) | Each crate's `tests/` | Deep engine/API contracts (index authority for nested spawn, resident, etc.) |
| P (process) | `crates/hya-e2e` | Real binary + FakeLlm: tools, permissions, skills, MCP, subagents, hyabundle |

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
| Skills and the trusted core Skill bundle | [Skills](skills.md) |
| Store/schema | [Storage](architecture/storage.md) |
| Server/client API | [Server and Client](architecture/server-client.md), [Protocol guide](protocol/README.md) |
| Bundle authoring and execution | [AgentBundle Authoring](agent-bundle-authoring.md), [Bundle Runtime](bundle-runtime.md), [Claude import](claude-plugin-import.md) |
| Trusted presets and multi-agent bundles | [Core agents](core-agents.md), [Tool-family presets](base-tools.md), [Subagent bundles](subagent-bundles.md), [Agent channels](agent-channels.md) |
| Goal/loop intelligence | [Goal and Loop Authoring](goal-loop-authoring.md) |
| Agent process E2E / matrix | [Testing](testing/README.md), [Agent matrix](testing/agent-matrix.md) |

Every new or modified feature ships with its documentation in the same change.
The feature's documentation must state, at minimum:

1. **Introduction** — what the feature does and why it exists.
2. **Usage** — how to invoke or configure it: CLI commands and flags, config
   keys, TUI keys or slash commands, plus a short worked example.
3. **Interface definition** — the exact contracts it exposes: HTTP/RPC routes
   with request/response schemas, event and payload types, tool names and
   parameter schemas, or config field names and types.

Keep docs grounded in shipped behavior. If a table or schema reserves space for
future functionality that is not wired into the current read path, say that
plainly.
