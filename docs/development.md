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

## Embedded runtime shutdown

`hya_app::build_session_engine` returns a `BuiltSessionEngine` that owns the
composition tasks. Keep it alive while using its engine, then call
`built.shutdown().await?`, including on error paths. This cancels intake and
admitted Workflow work and waits for the spawn/Workflow workers,
mailbox/lifecycle services and their request handlers, deferred MCP preparation,
and catalog scope sweeper. It then drains turns and residents, after every
admitted registration has settled, joins the resident listener and all episode,
main and team queue tasks, then stops sources and joins plugin/MCP connection
I/O. Both engine supervisor references are weak. Runtime teardown terminates processes after resident drain; explicit
`PluginHost::shutdown` and `PreparedPlugin::shutdown` keep the graceful shutdown
RPC. Source owners remain reachable for shutdown through weak references even
when only an older turn binding retains their generation. Retaining an engine
or control clone does not keep those services running after shutdown. Dropping the built value signals
cancellation without waiting; explicit shutdown is the completion boundary.

```rust,ignore
let mut built = hya_app::build_session_engine(store, router, &agent, mcp, plugins, tools).await?;
let result = use_engine(built.engine()).await;
built.shutdown().await?;
result
```

For a server embedder, construction stages fenced recovered actors without
running them. Build its AppState and call the same startup helper as the daemon
before advertising readiness:

```rust,ignore
let state = hya_server::AppState::new(built.engine(), agent);
let restored = built.resume_interrupted(&state).await?;
// Serve the state, then use the ordered shutdown above.
```

The Rust startup contracts are
`BuiltSessionEngine::resume_interrupted(&self, state: &AppState) -> Result<Vec<SessionId>, CoreError>`
and `ResidentSupervisor::start_recovered(&self) -> Result<usize, CoreError>`
(both async). The first admits root restores then invokes the second; the count
is the number of staged episodes installed. Claims ended or replaced by restore
tools before installation stay fenced and are skipped. A lower-level embedder explicitly
calls the second after admitting its own restores. The revival seam is
`SessionEngine::set_reviver(Weak<ResidentSupervisor>) -> ()`.

For server restores,
`SessionEngine::run_admitted_turn(lease: TurnLease, agent: &AgentSpec, cancel: CancellationToken, external_dirs: &[PathBuf], guidance: Option<Arc<str>>, explicit_model: Option<ModelRef>, admitted_prompt: MessageId) -> Result<FinishReason, CoreError>`
is async and consumes the lease reserved before admission. The lease identifies
the session and remains owned through the turn's message closure. Ordinary
external turn entry points retain their existing admission behavior. No CLI
flags, configuration fields, RPCs or event payloads change.

The lower-level `spawn_team_supervisor(...) -> RuntimeTaskOwner` also requires
retaining its returned owner and awaiting `owner.shutdown()`. Its sidecar
environment carries the owner's connection I/O tracker, including initialization
that never publishes. Shutdown stops intake and joins registrations, drains
residents, stops runtime sources, then joins tracked connection I/O. Drop only
signals this cleanup; await shutdown before releasing resources. Core service
contracts are `run_mailbox_service(engine, receiver, stop)` and
`run_lifecycle_service(engine, supervisor, receiver, stop)`, with
`stop: CancellationToken`. Cancellation stops intake, cancels pending handlers,
and joins them before returning. Workflow control rejects commands after
shutdown with the existing `WORKFLOW_RUNTIME_UNAVAILABLE` code. No CLI flags
or wire schemas change.

Connection owners expose explicit completion boundaries too:
`PluginClient::shutdown(&self) -> ()`, `PreparedPlugin::shutdown(&self) ->
Result<(), PluginError>`, `McpClient::shutdown(&self) -> ()`,
`PreparedMcpServer::shutdown(&self) -> ()`, and `McpManager::shutdown(&self) -> ()`
are async methods. Plugin stdout, reverse capability handlers, event delivery,
and MCP stdio/classic SSE readers are joined by those owners. Streamable HTTP
requests run in the caller's future. For example, an embedder that calls
`hya_mcp::prepare(name, config).await?` must later await `server.shutdown()`
before dropping its resources. Runtime sources implement
`RuntimeSourceOwner::shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>`;
static owners do no work. A custom registry embedder calls
`registry.shutdown_sources().await` after stopping preparation and draining its
activations. Retained tool views stay closed after owner shutdown.

The existing runtime task owner also tracks I/O completion during initialization,
before a source can be published. `hya_mcp::io_tasks::scope(tracker, future)` carries
`Option<tokio_util::task::TaskTracker>` through preparation; `current()` captures
it for connection restarts/cleanup, and `spawn(future)` / `spawn_in(tracker, future)`
return the owned `JoinHandle<F::Output>`. These helpers track completion without
owning cancellation. The runtime waits for this barrier after stopping source
owners, including cleanup from canceled deferred initialization.

For an embedded headless root driver, hold
`ResidentSupervisor::root_driver_activity(session: SessionId) -> Result<RootDriverActivity, CoreError>`
for the entire run. The guard reuses the Workflow activity counter: while held,
the supervisor neither starts root mail turns nor admits quiescence synthesis
prompts. Member turns still run. For example, `exec` holds this guard while its
lead loop checks live members, unread root mail, todos and final-step output;
only that loop injects continuation system notices. A cold root stays
non-autonomous when the guard drops; a later externally started root turn
restores normal resident supervision.

The shared transcript reducer records `MessageProjection.last_step_start:
Option<usize>` as the part offset at the newest `StepStarted` for that message.
It is absent before a step starts and omitted from JSON when absent. For
example, after `[Tool]`, a new step records offset `1`; a subsequent text part
belongs to that step, while a reasoning-only step yields no final text. A tool
anywhere within that step excludes its text from headless markup/announcement
checks. `PartDeleted` adjusts the offset when an earlier part is removed.
The field survives snapshot/replay; reducer version `6` invalidates older cached
projections. `StepStarted` does not change message content timestamps, and no
RPC or event payload fields change.

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
| `proto/hya/v1` | Regenerate with `cargo run -p xtask -- gen-api`, then `hya-api`, `hya-server`, `hya-sdk-v1`, and `packages/hya-tui` when its generated catalog or protobuf definitions change |
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
[agent feature matrix](testing/agent-matrix.md). CI enforces Track P in
[`ci.yml`](../.github/workflows/ci.yml).

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
  `[workspace.package].version` and `hya-backend` package mirror it. Backend
  releases use the `backend/<version>` tag (no `v` prefix) and
  `CHANGELOG_BACKEND.md`; its first heading is exactly `# <backend-version>`.
- `[frontend].version` is the frontend release version and MAY differ from the
  backend. Frontend releases use the `frontend/<version>` tag (no `v` prefix)
  and `CHANGELOG_FRONTEND.md`. `packages/hya-tui/frontend-version.ts` embeds
  it together with `minimumBackendVersion`; a frontend-only release does not
  bump the backend workspace or package version.

The frontend compatibility contract is inclusive: it accepts a backend only
when `backend >= frontend.minimum_backend_version`; missing, malformed, or
older backend versions are rejected during bootstrap, server switching, and
remote entry. Raise the minimum only when a frontend change requires a newer
backend contract.

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
`CHANGELOG.md`.

## Publishing a release

`.github/workflows/release.yml` publishes only when a side tag is pushed:

| Side | Tag | Archive | Installer delivery | Payload |
| --- | --- | --- | --- | --- |
| Backend | `backend/<version>` | `hya-backend-<version>-<target>.tar.gz` | Hosted `scripts/hya-install.sh`; no installer asset | `bin/hya`, Bun, Bun adapter, first-party bundles; no TUI/WebUI |
| Frontend | `frontend/<version>` | `hya-frontend-<version>-<target>.tar.gz` | Same hosted installer with `--tui-only`; no installer asset | Bun, TUI, WebUI; no `bin/hya` |

The single installer is served at `https://hya.ed-aisys.com/install.sh` from
`main` and embedded in `hya update`. It installs both sides by default, or one
side with `--backend-only`/`--tui-only`, verifies each selected side against
its `SHA256SUMS`, and rolls back a failed side. Release download paths use the
exact side tag and version; do not use `v`-prefixed tags or
`/releases/download/<side>/v<version>/...` URLs.

The supported targets are `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, and `aarch64-apple-darwin`. Each side publishes
its own archive, checksums, release notes, and provenance attestations; the
release itself never carries an installer script.

Before publishing, run the checks for every side being shipped:

```sh
cargo test -p xtask -p hya-bundle
actionlint .github/workflows/release.yml
bash tests/hya_install_script.sh
cargo run -p xtask -- release-rehearsal \
  --component backend \
  --version <backend-semver> \
  --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --no-publish
# Repeat with --component frontend and --version <frontend-semver> when shipping the frontend.
```

Push the version/changelog commit to `main` before creating tags. Publish only
the affected side; for a cross-contract change, create and push both tags:

```sh
git push origin main
git tag -a backend/<backend-version> -m "release backend <backend-version>"
git push origin backend/<backend-version>
git tag -a frontend/<frontend-version> -m "release frontend <frontend-version>"
git push origin frontend/<frontend-version>
```

Main-branch pushes, the scheduled workflow, and `workflow_dispatch` refresh
dependency caches only; they do not package or publish a release. After a tag
run, verify the matching GitHub Release assets, run the installer contract, and
exercise the hosted installer on a supported host. See [Install and update](install.md)
for user commands and [Quality Guidelines](spec/backend/quality-guidelines.md)
for the workflow contract.

`cargo test -p xtask` validates aggregate/reference metadata, both changelog
headings, frontend minimum-backend compatibility, and release layout.
`cargo test -p hya-bundle` validates bundle preparation; release rehearsal
validates the lockfile, package layout, checksums, installer, and archive
contents.


## Dev tasks (`xtask` package)

`crates/xtask` is **dev-only tooling** and is not part of any shipped binary.
There is **no** Cargo alias named `xtask` in this workspace: invoke it as
`cargo run -p xtask -- <task> …`. The binary uses a hand-rolled positional
dispatcher (not clap): the first positional argument selects the task and every
remaining argument is forwarded verbatim. The currently supported tasks are
`startup-bench`, `lifecycle-bench`, `matrix-check`, `package-bundle`, `package-native-tool-bundle`,
`package-native-tool-library`, `stage-first-party-bundles`, `gen-api`, and
`release-rehearsal`.

| Task | Role |
| --- | --- |
| `gen-api` | Regenerate the `hya.v1` contract from `proto/hya/v1`: Rust types (prost/tonic/pbjson), the API reference, and OpenAPI. Uses a vendored protoc; output is committed, and the task fails when any rpc lacks its `// hya.http:` mapping or two rpcs collide. |
| `startup-bench` | Startup latency benchmark. Honours `HYA_BACKEND_BIN` to select the binary under test. `--db <seed.db>` runs every sample against a fresh copy of an existing database and prints the `HYA_STARTUP_TRACE` phase waterfall; `--timeout-secs N` (default 30) bounds the wait for the listen line. |
| `lifecycle-bench` | Offline app-wired task lifecycle timings and store counts. See [Lifecycle benchmark](#lifecycle-benchmark). |
| `matrix-check` | Validates `crates/hya-e2e/matrix.toml`. See [agent-matrix.md](testing/agent-matrix.md). |
| `package-bundle` | Validates a source directory and atomically writes the canonical deterministic public `.hyabundle` package. |
| `package-native-tool-bundle` | Adds a built target-specific Rust executable and exact policy tool declarations to one tool-family source, then writes a deterministic public package. |
| `package-native-tool-library` | Adds a built tool-family dynamic library and exact policy tool declarations to one tool-family source, then writes a deterministic public package. |
| `stage-first-party-bundles` | Packages the twelve trusted first-party bundles into a backend package's `<package-root>/bundles/` and fails if any bundle version differs from the backend release version. With `--target` and `--assets`, it also writes versioned standalone release assets. |
| `release-rehearsal` | Runs the pinned, non-publishing release build/package/smoke rehearsal for one target of one release side. It checks the aggregate version contract, then builds, packages, and smokes; the release workflow's own text is `actionlint`'s and the workflow's business, not a Rust linter's. Backend rehearsals cover the binary, Bun adapter, first-party bundles, checksums, unified installer contract, and headless smoke; frontend rehearsals cover Bun, TUI/WebUI assets, production dependencies, checksums, unified installer contract, and frontend smoke. Run it on a host of that target (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`). |

```sh
cargo run -p xtask -- matrix-check
cargo run -p xtask -- startup-bench
cargo run -p xtask -- package-bundle <source-dir> <output.hyabundle>
cargo run -p xtask -- package-native-tool-bundle <tool-family-source-dir> <built-executable> <output.hyabundle>
cargo run -p xtask -- package-native-tool-library <tool-family-source-dir> <built-library> <output.hyabundle>
cargo run -p xtask -- stage-first-party-bundles --library-dir target/release --package-root dist/hya [--version <backend-semver>] [--target <triple> --assets dist]
cargo run -p xtask -- release-rehearsal --component backend --version <backend-semver> --target "$(rustc -vV | sed -n 's/^host: //p')" --no-publish
cargo run -p xtask -- release-rehearsal --component frontend --version <frontend-semver> --target "$(rustc -vV | sed -n 's/^host: //p')" --no-publish
```

## Crate Selection

Use this guide when deciding where a change belongs:

| Change | Crate |
| --- | --- |
| New event, id, API DTO, message field, projection behavior | `hya-proto` |
| New provider route, protocol encoder/decoder, capability preflight | `hya-provider` |
| New builtin tool implementation | Owning source under `bundles/presets/<family>-tools/native` |
| Tool trait, loader, runtime plane, or permission action | `hya-tool` |
| Persistence, replay, migrations, usage projection | `hya-store` |
| Turn-loop behavior, goal/loop/team/worktree runtime logic | `hya-core` |
| HTTP route or SSE behavior | `hya-server` |
| `hya.v1` contract change (proto message/rpc, error code, HTTP binding) | `hya-api` — edit `proto/hya/v1/*.proto`, then regenerate with `cargo run -p xtask -- gen-api` |
| Typed Rust HTTP integration | `hya-sdk-v1` (`Client` for tooling; `V1Sdk` for frontends) |
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
| Standalone TUI extension SDK | [TUI Extension SDK](tui-extension-sdk.md) |

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

### Lifecycle benchmark

`CARGO_BUILD_JOBS=8 cargo run --locked -p xtask -- lifecycle-bench` measures
app-wired native `task` spawning, a read-tool turn with streamed text, terminal
report/archive, and mail-driven revival, with 1, 16 and 64 workers under one root.
It uses an offline fake provider and a temporary file-backed WAL SQLite database.
Workers pause at provider entry so spawning and revival can be measured separately
from their work. Completion is observed on the event bus, without polling SQL.
Prompt admission completes before that gate and belongs to `spawn`/`revive`;
`work` begins after admission. These cases cover first and revived activations.
Live mail wake coalescing is covered by the resident lifecycle regressions.

Output rows contain `scenario` (`A` initial episode, `B` revived episode), `n`,
`phase`, `wall_ms`, and `counts` (`statements`, `transactions`, `events`, `bytes`).
`scenario` and `phase` are strings, `n` is an unsigned integer, `wall_ms` is a
floating-point number, and each count is an unsigned integer. Phases are `spawn`,
`work`, `report+archive`, and `revive`; A has no revive row and B has no spawn row.
Use `--json` for a JSON array of these rows. Baselines use the same array with
`wall_ms` omitted. `--check` takes one baseline file path and exits nonzero on a
count mismatch; `--json` is a flag and can be combined with `--check`.
SQL statements include transaction
control; transactions count only commits containing event inserts; bytes count
UTF-8 event JSON, excluding SQLite framing, indexes and side tables.

Store instrumentation is compiled only with `hya-store/lifecycle-bench`, enabled
by xtask. SQLite trace callbacks count executed statements and event payloads;
commit/rollback observation distinguishes durable appends. Callbacks add counting
and allocation overhead, but execute no SQL and add no transactions. The product
code path stays the same. Callback overhead affects scheduling and wall time;
these are instrumented timings.
The optional direct `libsqlite3-sys` dependency uses SQLx's existing locked version
because SQLx exposes no statement-trace registration API.

`--check crates/xtask/lifecycle-baseline.json` compares events and append
transactions exactly;
timings are informational and never gate shared CI. SQL statement tolerance is
±(2 + N/16) per phase, using integer division: N=1 → 2, N=16 → 3, N=64 → 6.
Projection-tail reads can cross the asynchronous phase boundary, with more
variation at larger fan-out.
Payload byte tolerance is the empirical allowance ±`3*(n+1)`. Persisted tool
durations vary in decimal width. Concurrent handle assignment also changes the
root task's truncated output: clipping different text changes JSON escaping and
serialized byte length. The allowance covers both observed sources; it is not a
claim that payload sizes are deterministic. The raw counts stay visible;
we do not normalize stored payloads or suppress read statements. Changes beyond
these bounds fail; event and transaction differences always fail. After an
intentional lifecycle change, repeat five runs, investigate any count variation, then replace the baseline
with reviewed `--json` output. Compare timing min/median/max on the same machine;
relative spread is `(max-min)/median`, not a performance regression threshold.

### Server task shutdown

`hya_server::AppState` owns the live frame relay, catalog notice relay, Project
busy watcher, session-list tracker, ephemeral reaper (including startup sweep
and delayed checks), and permission/question bridges. Routers retain the owner;
background tasks retain only its cancellation and tracking handles, so dropping
the final owner can cancel them without an ownership cycle.

Shutdown keeps the interaction bridges alive until their turns have been
cancelled and drained. Embedders use this order:

```rust,ignore
state.begin_shutdown();
built.shutdown().await?;
state.shutdown().await;
```

`AppState::begin_shutdown(&self) -> ()` ends live streams and fences server
admission without stopping services. The built engine then closes engine
admission, joins composition intake/registration handlers, and drains residents.
`AppState::shutdown(&self) -> impl Future<Output = ()>` drains any remaining
turns and reserved server runs, then cancels and joins server services and their
tracked children. `AppState::stop_for_restart(Duration) -> Result<TurnDrainReport,
CoreError>` uses the same final drain/join sequence with the caller's deadline
and a `restart` stream-close reason. Server services are joined after the drain
wait, including when that wait reaches its deadline.

`HyaRuntime::shutdown(&mut self) -> Result<(), hya_core::CoreError>` performs
all three steps; the CLI serve path follows the same order. For example:

```rust,ignore
let mut runtime = hya_app::HyaRuntime::start(options).await?;
serve(runtime.router().clone()).await;
runtime.shutdown().await?;
```

The shared `RuntimeTaskOwner` remains available from `hya_app` and is defined
in `hya_server`; `stop: CancellationToken` and `tasks: TaskTracker` track service
cancellation and completion. Explicit shutdown joins; drop only cancels.
Resident bus listeners and plugin/MCP connection I/O have separate owners.
