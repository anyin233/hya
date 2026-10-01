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
- This workflow has no Trellis runtime dependency.

## Build and Quality Gate

Local verification is **per component**: run the checks for the crates and
packages a change touches, plus the components that consume a changed public
contract. CI ([`ci.yml`](../.github/workflows/ci.yml)) runs the full workspace
suite, Track P, and every TUI check on each push and pull request, so do not
repeat the full suite locally. Run `cargo test --workspace`,
`cargo clippy --workspace`, the whole `hya-e2e` matrix, or the whole Playwright
suite only when asked, or for a cross-cutting change that touches most crates
(for example a workspace-wide dependency or lint bump).

### Choosing the affected components

| Change | Verify |
| --- | --- |
| Private to one crate or package (internal functions, tests, non-`pub` items) | That crate or package only |
| Public API, wire types, or behavior other crates rely on | The crate and its direct workspace dependents that use the changed items |
| `proto/hya/v1` | Regenerate with `cargo run -p xtask -- gen-api`, then `hya-api`, `hya-server`, `hya-sdk-v1`, `hya-client`, and `packages/hya-tui` when its generated catalog or protobuf definitions change |
| Version bump ([Version bumps](#version-bumps)) | `cargo test -p xtask` and `cargo test -p hya-bundle` |
| `crates/hya-e2e/matrix.toml` or a new Track P test | `cargo run -p xtask -- matrix-check` |
| Documentation only | No build or tests; check links and that no repository-private process notes leaked into project docs |
| CI only (`.github/`) | A syntax check of the edited workflow |

List a crate's direct workspace dependents with:

```sh
cargo tree --workspace -i <crate> --depth 1 -e normal,dev --prefix none
```

Foundation crates (`hya-proto`, `hya-api`, `hya-tool`) have many dependents;
test only the ones that use the changed items (find them through references),
not the whole list.

### Rust components

```sh
cargo fmt --all --check
cargo clippy -p <crate> [-p <dependent> ...] --all-targets -- -D warnings
cargo test -p <crate> [-p <dependent> ...]
cargo build -p hya-backend --bin hya
```

`cargo fmt --all --check` is cheap and stays workspace-wide. While iterating,
narrow further with `cargo test -p <crate> --test <name>` or a test-name
filter; run the affected crates' full tests before landing.

Any workspace-wide `cargo test` must pass `--exclude hya-e2e` (as CI does):
Track P spawns real backend processes and must not run multi-threaded.

### Process agent E2E (Track P)

Product-path coverage lives in `crates/hya-e2e` (real `hya` + FakeLlm). Run it
only when a change touches `crates/hya-e2e` or an agent surface the PR matrix
covers (permissions, skills, MCP, subagents, hyabundle), and run the scenario
files for that surface rather than the whole matrix. It needs a built backend
binary and always runs single-threaded:

```sh
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e --test <pNN_scenario> -- --test-threads=1
cargo clippy -p hya-e2e --all-targets -- -D warnings
```

See [Testing](testing/README.md), [Process E2E](testing/process-e2e.md), and the
[agent feature matrix](testing/agent-matrix.md). Optional CI wiring is sketched
in [ci-agent-e2e-snippet.yml](testing/ci-agent-e2e-snippet.yml).

### Bun unit tests in extra bundles

`hya-extra/*` bundles with Bun processes (for example
`bundles/extra/jev-model-router`) keep their unit tests in an undeclared,
never-packaged `*.test.ts` file beside the script. CI does not run them;
when you change such a bundle, run them from its directory:

```sh
cd bundles/extra/jev-model-router && bun test
```

### OpenTUI frontend

When you change `packages/hya-tui`, run from that directory (`bun test <file>`
narrows it while iterating):

```sh
cd packages/hya-tui
bun run typecheck
bun test
```

When you change the Bun adapter, run `bun run typecheck && bun test` from
`crates/hya-plugin-bun/adapter`.

### Browser-rendered TUI tests

`packages/hya-tui-web` renders a terminal frontend in Chromium through a real
PTY and xterm.js. When you change it, run its unit tests; when you make a
user-visible TUI change, also run the Playwright specs that cover it (the new or
changed spec and the specs for the screens it touches), not the whole suite:

```sh
cd packages/hya-tui-web
bun run typecheck
bun test ./test
bunx playwright test e2e/<spec>.ts
```

See [Browser-rendered TUI](tui-web.md) for the harness API. CI runs the full
suite (plus the `packages/hya-tui` and adapter typecheck/test above) in the
`tui` job of [`ci.yml`](../.github/workflows/ci.yml); see
[tui-web.md#ci](tui-web.md#ci).

## Version bumps

Backend and frontend release iterations are independent. `versions.toml` is the
aggregate source of truth:

- `[backend].version` is the backend release version. The root
  `[workspace.package].version`, the `hya-backend` package, the backend `vX.Y.Z`
  tag, and `CHANGELOG_BACKEND.md` mirror it. The backend changelog's first
  heading is exactly `# <backend-version>`.
- `[frontend].version` is the frontend release version and MAY differ from the
  backend. `packages/hya-tui/frontend-version.ts` embeds it together with
  `minimumBackendVersion`, and `CHANGELOG_FRONTEND.md` carries the frontend
  notes. The frontend changelog's first heading is exactly
  `# <frontend-version>`; a frontend-only release does not bump the backend
  workspace or package version.

The frontend compatibility contract is inclusive: it accepts a backend only
when `backend >= frontend.minimum_backend_version`; missing, malformed, or
older backend versions are rejected during bootstrap, server switching, and
remote entry. Raise the minimum only when a frontend change requires a newer
backend contract. The current split is frontend `0.43.40`, requiring backend
`0.43.41` or newer.

All other Rust package manifests use the placeholder `0.0.0` plus
`[package.metadata.hya] version-reference = "backend"`; frontend package
manifests use `0.0.0` plus the equivalent `frontend` reference. Bundle source
manifests use `version_ref: backend` and identity version `0.0.0`; preparation
resolves that reference to the backend aggregate, so prepared and released
bundles carry the actual backend version without editing every source manifest.

When changing shipped backend behavior, bump the backend aggregate and update
the backend changelog. When changing shipped frontend behavior, bump only the
frontend aggregate unless the compatibility minimum also needs to move. A
cross-contract change bumps each affected side once. Shipped behavior means
Rust crates, `proto/`, `bundles/`, `packages/hya-tui`, `packages/hya-tui-web`,
the Bun adapter, and anything else in the release archive or source install.

These changes do **not** bump either version or write a new side-specific
changelog:

- documentation only: `docs/`, `*.md` files, code comments, `AGENTS.md`,
  `.planning/`;
- CI only: `.github/`, CI scripts and config;
- tests only, when no shipped code changes.

Each side-specific changelog contains only its newest notes. When a side
advances, archive its previous file as
`docs/changes/CHANGELOG_BACKEND_<version>.md` or
`docs/changes/CHANGELOG_FRONTEND_<version>.md`; never recreate the old
`CHANGELOG.md`. `cargo test -p xtask` validates both changelog headings,
aggregate/reference metadata, frontend minimum-backend compatibility, and
release layout. `cargo test -p hya-bundle` validates bundle preparation, while
release rehearsal validates the lockfile, package layout, and archive copies.

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
| `startup-bench` | Startup latency benchmark. Honours `HYA_BACKEND_BIN` to select the binary under test. `--db <seed.db>` runs every sample against a fresh copy of an existing database and prints the `HYA_STARTUP_TRACE` phase waterfall; `--timeout-secs N` (default 30) bounds the wait for the listen line. |
| `matrix-check` | Validates `crates/hya-e2e/matrix.toml`. See [agent-matrix.md](testing/agent-matrix.md). |
| `package-bundle` | Validates a source directory and atomically writes the canonical deterministic public `.hyabundle` package. |
| `package-native-tool-bundle` | Adds a built target-specific Rust executable and exact policy tool declarations to one tool-family source, then writes a deterministic public package. |
| `package-native-tool-library` | Adds a built tool-family dynamic library and exact policy tool declarations to one tool-family source, then writes a deterministic public package. |
| `stage-first-party-bundles` | Packages the eleven trusted first-party bundles into `<package-root>/bundles/` and fails if any bundle version differs from the release version. With `--target` and `--assets`, it also writes each package as a versioned standalone release asset. The release workflow, the rehearsal, and `install.sh` all use it. |
| `release-rehearsal` | Runs the pinned, non-publishing release build/package/smoke rehearsal for one target of the release matrix, including archive, first-party bundle assets, checksums, the bundled Bun (`lib/hya/bin/bun`), adapter, TUI/WebUI assets (`lib/hya/tui` with the host's OpenTUI native package, `lib/hya/tui-web` without dev dependencies, `--help` smoke from outside the checkout, the WebUI page and bundled assets served, and every import resolving inside the staged directories), Argus checks, and an install of the archive with `scripts/hya-install.sh` from a `file://` release tree followed by bare `hya update` ([install.md](install.md)). Run it on a host of that target (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, or `x86_64-apple-darwin`); it needs `actionlint` 1.7.12, Bun 1.4.2, 7-Zip (`7z`), and `shasum` on `PATH`. |

```sh
cargo run -p xtask -- matrix-check
cargo run -p xtask -- startup-bench
cargo run -p xtask -- package-bundle <source-dir> <output.hyabundle>
cargo run -p xtask -- package-native-tool-bundle <tool-family-source-dir> <built-executable> <output.hyabundle>
cargo run -p xtask -- package-native-tool-library <tool-family-source-dir> <built-library> <output.hyabundle>
cargo run -p xtask -- stage-first-party-bundles --library-dir target/release --package-root dist/hya [--version <semver>] [--target <triple> --assets dist]
cargo run -p xtask -- release-rehearsal --workflow .github/workflows/release.yml --version 0.42.0 --target "$(rustc -vV | sed -n 's/^host: //p')" --no-publish
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
| Typed Rust HTTP integration | `hya-client`; Rust frontends use `hya-sdk-v1` |
| Bun/OpenTUI frontend behavior | `packages/hya-tui` (v1 HTTP/JSON+SSE and direct gRPC client) |
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
| Optional `hya-extra/*` distribution bundles | [Extra bundles](extra-bundles.md) |
| Goal/loop intelligence | [Goal and Loop Authoring](goal-loop-authoring.md) |
| Agent process E2E / matrix | [Testing](testing/README.md), [Agent matrix](testing/agent-matrix.md) |
| OpenTUI frontend | [OpenTUI frontend](tui.md) |
| Browser-rendered TUI, WebUI host, TUI browser tests | [Browser-rendered TUI](tui-web.md) |

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
