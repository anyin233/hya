# Task Management

Use `planning-with-files` for repository task management. For multi-step work or
cross-session recovery, keep `task_plan.md`, `findings.md`, and `progress.md` in
`.planning/<YYYY-MM-DD-slug>/`. Small tasks may use a lightweight plan.

- Resume the relevant existing plan; keep unrelated task directories intact.
- `.planning/.active_plan` is an optional pointer to the current plan when several
  plan directories coexist. Prefer an explicit task or `PLAN_ID` when several
  sessions are active.
- Record phase status, decisions, blockers, and verification results as work
  progresses. Recover context from these files when resuming.
- Use the installed skill when available; the Markdown files remain usable
  directly without a plugin or CLI. No content-hash approval gate is required.
- Before editing a layer, read its guideline index under `docs/spec/backend/`;
  consult `docs/spec/guides/index.md` for cross-layer changes and code-reuse
  decisions.
- `docs/development-history/` preserves prior tasks and journals as historical
  evidence, not active workflow instructions. Bring relevant unfinished work
  into a planning directory when explicitly resumed.

## Commit Rule

- When the user explicitly asks for commits, create one git commit per atomic change before reporting done; for verified feature work, commit and push the atomic change before reporting done.
- Stage only the files for that atomic change; never sweep in unrelated workspace changes.
- Use one-line semantic commit messages with no agent or AI attribution.
- Do not commit or push feature work until its required TDD test and verification gate have passed.

## Feature Workflow Rule

- For every user-requested feature, follow TDD: add one atomic failing test first, verify it fails for the expected missing behavior, implement the smallest change that passes, then run the required verification gate for the touched area.
- After the implementation is verified, the agent must commit and push the atomic feature change.
- If the feature cannot be verified, do not commit or push; report the blocker and the commands or checks that failed.

## Feature Documentation Rule

- Every new or modified feature must ship with its documentation in the same atomic change; a feature is not done until its docs exist.
- The documentation must cover three parts: an introduction (what the feature does and why it exists), usage (how to invoke or configure it — CLI commands and flags, config keys, TUI keys or slash commands, and a short worked example), and interface definitions (the exact contracts it exposes — HTTP/RPC routes with request/response schemas, event and payload types, tool names and parameter schemas, or config field names and types).
- Place the documentation using the boundary-to-page table in `docs/development.md`; a genuinely new surface gets a new page under `docs/` linked from `docs/README.md`.
- Documentation is part of the feature's verification gate: do not commit or push feature work until the matching documentation is updated.

## TUI Preview & Browser Test Rule

The TUI and the WebUI are one frontend: `packages/hya-tui-web` runs the TUI on
a real PTY and renders it in the browser with xterm.js (see `docs/tui-web.md`,
ADR-0018). All TUI preview and testing goes through that browser rendering.

- **Preview in the browser, not a terminal multiplexer.** Do not use tmux,
  `script`, or terminal scraping to check TUI output. To look at the TUI, serve
  it and open the printed URL, for example:
  `HYA_BIN=target/debug/hya bun packages/hya-tui-web/src/main.ts --port 7681 -- bun packages/hya-tui/src/main.ts --dir "$PWD"`
  (the TUI starts and stops its own `hya serve`), or add
  `--server http://127.0.0.1:8080` to the TUI command to use a
  `hya serve --bind 127.0.0.1:8080` you run yourself. To preview bare `hya`
  itself (TUI + WebUI), make it the host command:
  `bun packages/hya-tui-web/src/main.ts --port 7681 -- target/debug/hya --port 0`.
  The offline echo model is enough; do not spend real provider calls on UI
  checks.
- **Every user-visible TUI change gets a Playwright spec** under
  `packages/hya-tui-web/e2e/`. Use the `tui()` fixture from `e2e/harness.ts`,
  and follow the TDD gate: the spec fails before the change. Cover layout,
  keys, resize, and exit paths the change touches.
- **Assert on the terminal buffer, not pixels.** Use `waitForText`, `find`,
  `cell` (color `#rrggbb`, glyph `width`, bold/inverse), and `size`. Do not
  commit pixel-baseline screenshots, because fonts differ between machines.
  Use `waitForText`/`expect.poll`, never fixed sleeps.
- **Look at the result before reporting a visual change done.** Every test
  writes `test-results/<test>/final-screen.png` and `final-screen.txt`. Open
  the PNG (agents: read the image) at the default 1100×640 viewport. If the
  change depends on size, also check a narrow viewport (about 80 columns).
- **Keep keybindings browser-safe.** The WebUI runs inside a browser, which
  reserves some shortcuts (Ctrl/Cmd+W, T, N, L, Tab, and Ctrl+Tab). Do not bind
  core TUI actions only to these. Every binding must be reachable through
  `press()` in a spec.
- **Keep the host generic.** `packages/hya-tui-web` has no hya-specific logic
  and runs only the fixed command it was started with. The WebUI shows exactly
  what the TUI draws; a browser-only feature needs its own ADR. Rendering never
  moves into `hya serve`.
- **Frame contract.** The `/pty` WebSocket speaks the protojson
  `hya.v1` `PtyClientFrame`/`PtyServerFrame` shapes. A change to that contract
  updates `src/frames.ts`, its unit tests, and `docs/tui-web.md` together.

## Release & Changelog Rule

- Before publishing a new version, the local agent must ensure `[workspace.package].version` in `Cargo.toml`, the `vX.Y.Z` release tag, and root `CHANGELOG.md` all describe the same version.
- Every fix or feature change must include an explicit project version number update in `[workspace.package].version` in `Cargo.toml`; keep the release tag and changelog aligned when publishing.
- The twelve first-party bundles are released with hya: every `bundles/presets/*/bundle.yaml` and `bundles/first-party/*/bundle.yaml` identity `version` must equal `[workspace.package].version`. Bump them together; `stage-first-party-bundles` and the `hya-bundle` first-party test reject a mismatch.
- Root `CHANGELOG.md` must contain only the newest version's changelog because the GitHub release workflow reads it verbatim as the GitHub Release notes.
- When a previous root changelog exists, move it to `docs/changes/CHANGELOG_<version>.md` before writing the new root `CHANGELOG.md`.
- Historical changelog files stay under `docs/changes/`; do not append old release history back into root `CHANGELOG.md`.

## Project Overview

`hya` is a Rust multi-agent coding agent. It is built as an event-sourced
workspace: user prompts, model deltas, tool calls, permissions, token usage, and
session lifecycle changes are appended as `Event`s, then replayed into a
projection for the HTTP API and client surfaces. The interactive
frontend is the Bun/OpenTUI TUI in `packages/hya-tui` (v1 HTTP/JSON+SSE
client); `packages/hya-tui-web` renders the same TUI in a browser as the
WebUI. Bare `hya` on a terminal starts both against an in-process server.
`hya-sdk-v1`, `hya-client`, and gRPC are the other supported ways to drive a
backend.

The server exposes exactly one contract — `hya.v1` (17 services / 89 rpcs in
`proto/hya/v1`) — over HTTP/JSON+SSE+WebSocket under `/v1` and, when
`HYA_GRPC_BIND` is set, over gRPC through `hya_server::V1Grpc`, which dispatches
through the same router. The legacy Compat `/api/*`, bare native routes, and
old `/sessions/*` surface are deleted.

The main runtime path is:

```text
hya / hya-server
  -> hya-app config/auth/plugin/MCP composition and WorkflowControl
  -> hya-workflow compiled/normalized Workflow plans
  -> hya-core::SessionEngine and durable Workflow execution
  -> hya-provider streaming model route
  -> hya-tool builtin, MCP, or plugin tools
  -> hya-store SQLite event log
  -> v1 clients (hya-sdk-v1, hya-client, gRPC) over the same projection
```

Workflow authoring and normalization live in `hya-workflow`; durable execution
lives in `hya-core`, and cross-surface admission/control lives in `hya-app`.

The engine owns stop decisions. Goal mode and loop mode use separate evaluators
or verifiers; workers do not decide that their own objective is done.

## Component Map

| Component | Feature |
| --- | --- |
| `crates/hya-backend` | Package for the unified `hya` executable — the only shipped binary and the single terminal entry point; subcommands select the controlled area. Bare `hya` on a terminal runs the v1 server in-process and starts the Bun TUI and the WebUI host as child processes (`src/frontend.rs`: asset/Bun resolution, lifecycle, log file; `--port`, default 3250; see `docs/cli.md` "Bare `hya`", ADR-0020); without a terminal it prints a guidance banner. Subcommands cover `exec`/`run`, `-p/--prompt` goal mode, `loop`, `serve`, `tail-session`, `sessions`, `rpc`, `login`/`oauth`/`auth`, `agent`, `bundle`, `workflow`, `models`, and `update` (the self-update TCB from `hya-updater`, dispatched before any runtime composition). Runtime commands **compose** through `hya-app`. Build with `cargo build -p hya-backend --bin hya`. |
| `crates/hya-app` | Runtime composition library (not a binary). Config load, provider/auth resolution, MCP and plugin wiring, permission policy construction, session engine build, `WorkflowControl` admission/list/info/select/run/state, and installed-bundle catalog refresh. Prefer this crate over `hya-backend` when changing composition or Workflow control, not CLI surface. |
| `crates/hya-bundle` | `AgentBundle` and `WorkflowBundle` prepare/validate/catalog types and package fixtures. Catalog builders and resource/agent/Workflow resolution used by install CLI and process E2E. Also the runtime loader for the twelve trusted first-party bundles (`first_party_bundle`; see `docs/bundle-runtime.md`). Prefer this crate for bundle authoring contracts and prepare semantics. |
| `crates/hya-workflow` | Workflow source parsing, normalization, validation, and immutable compiled plans. Prefer this crate for authoring/compile contracts; execution belongs to `hya-core`. |
| `crates/hya-core` | Agent runtime. Owns `SessionEngine`, turn admission, streaming rounds, shell turns, event bus, prompt construction, compaction, durable Workflow execution/replay, goal/loop drivers, hook dispatch, subagents, team state, worktree/tmux helpers, and session forking. |
| `crates/hya-proto` | Shared wire/domain types. Defines newtyped IDs, tagged `Event`/`Envelope`, messages, parts, roles, model/tool schema types, API DTOs, and the deterministic projection reducer. Keep this dependency-light so UI/client crates can reuse it cheaply. |
| `crates/hya-provider` | Model provider abstraction. Normalizes OpenAI-compatible, OpenAI Responses, OpenAI Codex, Grok Build, Anthropic, Google, dev, and fake routes into one streamed `Event` model; handles protocol encoding/decoding, provider routing, capability metadata, reasoning effort, and preflight checks for tool-capable routes. |
| `crates/hya-tool` | Tool and permission plane. Provides the `Tool` trait, the 28-name canonical registry (namespaced `ns__tool` names included) plus hidden aliases, allow/ask/deny rules, interaction/question requests, spawn/todo/skill/websearch/LSP/Workflow planes, and the lockstep native loader. The builtin tool implementations live in the five tool-family bundles under `bundles/presets/*-tools/native`. |
| `crates/hya-store` | Persistence. Stores events and token ledger entries in SQLite, runs migrations, lists/deletes sessions, replays event logs, and folds projections on read through `hya-proto::Projection`. |
| `crates/hya-server` | The `/v1` contract surface over `hya-core`: HTTP/JSON+SSE+WebSocket routes generated from the `hya.v1` IDL, plus `V1Grpc` (tonic) dispatching through the same router. Shared catalogs/guidance/PTY/worktree/git helpers live in `support`. The legacy Compat and native routes are deleted. |
| `crates/hya-api` | The v1 dual-protocol contract crate: `proto/hya/v1` generated Rust types (prost/tonic/pbjson protojson), stable error-code table mapped to HTTP statuses and gRPC codes, and cursor helpers. Regenerate with `cargo run -p xtask -- gen-api` (vendored protoc; output committed). |
| `crates/hya-sdk-v1` | Typed SDK for new frontends on the v1 API: bootstrap, sessions, event-driven turns, transcript/todo reads, curated replay, interactions, live SSE `StreamFrame` subscription, and `V1SessionMirror` transcript folding. |
| `crates/hya-client` | Typed `reqwest` client for the v1 API: sessions, event-driven turns (admit+wait), curated and raw-envelope event replay, and pending-interaction list/respond. |
| `crates/hya-updater` | Independent self-update TCB library (verify signed metadata, stage generations, smoke, owner-gated activation) and the `hya update` command surface (`hya_updater::cli`). Must not depend on runtime crates. See `docs/self-update.md`. |
| `crates/hya-mcp` | MCP support. Implements the MCP protocol/client/manager and bridges MCP tools into `hya-tool` with namespaced `mcp__server__tool` names and permission checks. |
| `crates/hya-plugin` | Out-of-process plugin host. Owns the JSON-RPC stdio protocol, plugin client/host, manifest/config loading, command/tool dispatch, hook dispatcher bridge, permission bridge, and plugin-backed tool adapter. |
| `crates/hya-plugin-bun` | Bun extension adapter (`kind: bun`). The Rust crate exports `BUN_ADAPTER_VERSION`; the Bun adapter under `adapter/` loads bundle JS extensions (`--bundle-extension`/`--extension`), translates hya wire hooks/tools/events, and exposes the runtime over NDJSON JSON-RPC stdio. The OpenCode compat layer is deleted. |
| `crates/hya-plugin-example` | Placeholder stub binary (`fn main() {}`); does **not** speak the plugin protocol. Reserved for a future deterministic native-plugin QA fixture. For a real ABI reference, see `docs/plugin-protocol.md`. |
| `crates/xtask` | Dev-tooling entry point with working tasks: `startup-bench`, `matrix-check`, `package-bundle`, and `release-rehearsal`. |
| `crates/hya-e2e` | Process-level agent E2E harness (Track P): real `hya` + FakeLlm. Matrix in `matrix.toml`; docs under `docs/testing/`. |
| `packages/hya-tui` | Bun/OpenTUI TUI over the v1 HTTP/JSON+SSE contract: sessions, transcript, turns, pending interactions, models, Workflows, saved provider keys, slash completion, and a generic `/api` command view. Started by bare `hya` (`--server <in-process URL> --web-url|--web-error`); shipped as `lib/hya/tui` in the release archive and source install, so it must stay self-contained (no imports outside the package; the `/api` catalog `src/operations.json` is generated by `gen-api`). Also runs directly with Bun for development (`bun packages/hya-tui/src/main.ts`). See `docs/tui.md`. |
| `packages/hya-tui-web` | Bun host that runs a terminal frontend on a real PTY and renders it in the browser with xterm.js (WebSocket frames reuse `hya.v1` `PtyClientFrame`/`PtyServerFrame`). Playwright harness for TUI visual/interaction tests and the WebUI host that bare `hya` starts (shipped as `lib/hya/tui-web`). See `docs/tui-web.md`. |
| `.planning` | Local task plans, findings, and progress using `planning-with-files`; existing tasks remain separate. |
| `docs/spec` | Project coding guidelines. Read the relevant layer's `index.md` before changing code. |
| `docs/development-history` | Preserved task artifacts and developer journals for historical reference. |
| `docs` | Project documentation: user guides, architecture, the `hya.v1` protocol references, historical Compat parity record, and testing/agent matrix under `docs/testing/`. |

## Change Guidance

- Rust workspace uses edition 2024 and `rust-version = "1.91"`.
- Library crates deny `unwrap_used` and `expect_used`; keep panic paths out of
  library code and use typed errors where the crate already has one.
- Preserve the event-sourced architecture: append events, replay with the shared
  projection, and avoid parallel read-model logic that can drift from replay.
- Keep `hya-proto` free of heavy runtime dependencies.
- TUI work goes into `packages/hya-tui` (ADR-0019). Do not add another
  interactive frontend (a Rust TUI crate, ratatui frontend, or second
  TypeScript terminal frontend) or move rendering into the backend without an
  explicit decision. Bare `hya` only orchestrates the Bun processes
  (ADR-0020); it never renders. The TUI reads the server projection over the v1 contract;
  it does not build a competing durable state model from SSE deltas.
- Prefer existing planes (`PermissionPlane`, `InteractionPlane`, `SpawnerPlane`,
  `TodoPlane`, `SkillPlane`, `WebSearchPlane`, `LspPlane`) over adding another
  cross-cutting runtime channel.
- For TypeScript adapter work, keep it under
  `crates/hya-plugin-bun/adapter` and use the existing Bun/TypeScript
  scripts instead of adding another JS toolchain.

## Verification

- After any fix, feature, or refactor, run the CI-equivalent checks for the touched areas and build a local executable before reporting done.

For Rust changes, run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --jobs 1 --exclude hya-e2e
```

Exclude `hya-e2e` from the default workspace suite (matches CI): Track P spawns
real backend processes and must not run multi-threaded under the default suite.

For process agent E2E (`crates/hya-e2e`) or agent-surface features that must not
regress the PR matrix (permissions, skills, MCP, subagents, hyabundle), also:

```sh
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e -- --test-threads=1
```

Matrix and harness docs: `docs/testing/README.md`, `docs/testing/agent-matrix.md`,
`docs/testing/process-e2e.md`, `crates/hya-e2e/matrix.toml`.

For Bun adapter changes, also run from
`crates/hya-plugin-bun/adapter`:

```sh
bun run typecheck
bun test
```

For OpenTUI frontend changes, run from `packages/hya-tui`:

```sh
bun run typecheck
bun test
```

For OpenTUI frontend and browser-rendered TUI changes, also run from
`packages/hya-tui-web`:

```sh
bun run typecheck
bun test ./test
bunx playwright test
```
