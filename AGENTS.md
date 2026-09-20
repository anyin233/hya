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
- Before editing a layer, read its guideline index under `docs/spec/backend/`
  or `docs/spec/frontend/`; consult `docs/spec/guides/index.md` for cross-layer
  changes and code-reuse decisions.
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

## Release & Changelog Rule

- Before publishing a new version, the local agent must ensure `[workspace.package].version` in `Cargo.toml`, the `vX.Y.Z` release tag, and root `CHANGELOG.md` all describe the same version.
- Every fix or feature change must include an explicit project version number update in `[workspace.package].version` in `Cargo.toml`; keep the release tag and changelog aligned when publishing.
- Root `CHANGELOG.md` must contain only the newest version's changelog because the GitHub release workflow reads it verbatim as the GitHub Release notes.
- When a previous root changelog exists, move it to `docs/changes/CHANGELOG_<version>.md` before writing the new root `CHANGELOG.md`.
- Historical changelog files stay under `docs/changes/`; do not append old release history back into root `CHANGELOG.md`.

## Project Overview

`hya` is a Rust multi-agent coding agent. It is built as an event-sourced
workspace: user prompts, model deltas, tool calls, permissions, token usage, and
session lifecycle changes are appended as `Event`s, then replayed into a
projection for the TUI, HTTP API, and client surfaces.

The server exposes exactly one contract — `hya.v1` (16 services / 79 rpcs in
`proto/hya/v1`) — over HTTP/JSON+SSE+WebSocket under `/v1` and, when
`HYA_GRPC_BIND` is set, over gRPC through `hya_server::V1Grpc`, which dispatches
through the same router. The legacy Compat `/api/*`, bare native routes, and
old `/sessions/*` surface are deleted.

The main runtime path is:

```text
hya -> hya-ts launcher/supervisor -> hya-backend runtime + hya.v1 HTTP/SSE server
  -> packages/hya-tui-ts TypeScript/OpenTUI frontend (legacy; see below)
hya-backend / hya-server
  -> hya-app config/auth/plugin/MCP composition and WorkflowControl
  -> hya-workflow compiled/normalized Workflow plans
  -> hya-core::SessionEngine and durable Workflow execution
  -> hya-provider streaming model route
  -> hya-tool builtin, MCP, or plugin tools
  -> hya-store SQLite event log
  -> v1 clients (hya-sdk-v1, hya-client, gRPC) over the same projection
```

The current `packages/hya-tui-ts` frontend was built on the deleted Compat SDK
(`@opencode-ai/sdk`) and is deliberately broken at runtime; it will be replaced
by a new TUI built on `hya-sdk-v1`. Until then, `hya-sdk-v1`, `hya-client`, and
the gRPC surface are the supported ways to drive a backend.

Workflow authoring and normalization live in `hya-workflow`; durable execution
lives in `hya-core`, and cross-surface admission/control lives in `hya-app`.

The engine owns stop decisions. Goal mode and loop mode use separate evaluators
or verifiers; workers do not decide that their own objective is done.

## Component Map

| Component | Feature |
| --- | --- |
| `crates/hya` | Canonical Unix entrypoint. Replaces itself with the adjacent `hya-ts` executable while preserving public `hya` branding; it does not contain a TUI or backend fallback. |
| `crates/hya-ts` | TypeScript TUI supervisor. Parses launcher/auth/import arguments, resolves the prepared runtime and backend, starts or attaches to `hya-backend`, and owns terminal process-group handoff and cleanup. |
| `packages/hya-tui-ts` | Legacy SolidJS/OpenTUI frontend. Still owns terminal rendering, routes, and command/keybinding UI, but its `@opencode-ai/sdk/v2` backend integration targeted the deleted Compat surface, so it is deliberately broken at runtime pending a replacement TUI built on `hya-sdk-v1`. |
| `crates/hya-backend` | Backend umbrella binary. Bare startup still launches the interactive TUI by spawning the current `hya` frontend, but does not own a terminal renderer. Also supports `exec`, `-p/--prompt` goal mode, `serve`, `tail-session`, auth/token commands, session listing, JSONL RPC, and CLI entry points that **compose** the runtime through `hya-app`. |
| `crates/hya-app` | Runtime composition library (not a binary). Config load, provider/auth resolution, MCP and plugin wiring, permission policy construction, session engine build, `WorkflowControl` admission/list/info/select/run/state, and installed-bundle catalog refresh. Prefer this crate over `hya-backend` when changing composition or Workflow control, not CLI surface. |
| `crates/hya-bundle` | `AgentBundle` and `WorkflowBundle` prepare/validate/catalog types and package fixtures. Catalog builders and resource/agent/Workflow resolution used by install CLI and process E2E. Prefer this crate for bundle authoring contracts and prepare semantics. |
| `crates/hya-workflow` | Workflow source parsing, normalization, validation, and immutable compiled plans. Prefer this crate for authoring/compile contracts; execution belongs to `hya-core`. |
| `crates/hya-core` | Agent runtime. Owns `SessionEngine`, turn admission, streaming rounds, shell turns, event bus, prompt construction, compaction, durable Workflow execution/replay, goal/loop drivers, hook dispatch, subagents, team state, worktree/tmux helpers, and session forking. |
| `crates/hya-proto` | Shared wire/domain types. Defines newtyped IDs, tagged `Event`/`Envelope`, messages, parts, roles, model/tool schema types, API DTOs, and the deterministic projection reducer. Keep this dependency-light so UI/client crates can reuse it cheaply. |
| `crates/hya-provider` | Model provider abstraction. Normalizes OpenAI-compatible, OpenAI Responses, OpenAI Codex, Grok Build, Anthropic, Google, dev, and fake routes into one streamed `Event` model; handles protocol encoding/decoding, provider routing, capability metadata, reasoning effort, and preflight checks for tool-capable routes. |
| `crates/hya-tool` | Tool and permission plane. Provides the `Tool` trait, the 28-name canonical registry (namespaced `ns__tool` names included) plus hidden aliases, allow/ask/deny rules, interaction/question requests, spawn/todo/skill/websearch/LSP/Workflow planes, and builtins for read/write/edit/apply-patch, Bash, web fetch/search, task/announce, Workflow, todo, and invalid-tool handling. |
| `crates/hya-store` | Persistence. Stores events and token ledger entries in SQLite, runs migrations, lists/deletes sessions, replays event logs, and folds projections on read through `hya-proto::Projection`. |
| `crates/hya-server` | The `/v1` contract surface over `hya-core`: HTTP/JSON+SSE+WebSocket routes generated from the `hya.v1` IDL, plus `V1Grpc` (tonic) dispatching through the same router. Shared catalogs/guidance/PTY/worktree/git helpers live in `support`. The legacy Compat and native routes are deleted. |
| `crates/hya-api` | The v1 dual-protocol contract crate: `proto/hya/v1` generated Rust types (prost/tonic/pbjson protojson), stable error-code table mapped to HTTP statuses and gRPC codes, and cursor helpers. Regenerate with `cargo run -p xtask -- gen-api` (vendored protoc; output committed). |
| `crates/hya-sdk-v1` | Typed SDK for new frontends on the v1 API: bootstrap, sessions, event-driven turns, transcript/todo reads, curated replay, interactions, live SSE `StreamFrame` subscription, and `V1SessionMirror` transcript folding. |
| `crates/hya-client` | Typed `reqwest` client for the v1 API: sessions, event-driven turns (admit+wait), curated and raw-envelope event replay, and pending-interaction list/respond. |
| `crates/hya-sdk` | Legacy SDK for the retired Compat surface (old TUI only); slated for deletion at the new-TUI cutover. New frontends use `hya-sdk-v1`. |
| `crates/hya-native` | Legacy in-process transport for the retired Compat surface (old TUI only). |
| `crates/hya-updater` | Independent self-update TCB (verify signed metadata, stage generations, smoke, owner-gated activation). See `docs/self-update.md`. |
| `crates/hya-mcp` | MCP support. Implements the MCP protocol/client/manager and bridges MCP tools into `hya-tool` with namespaced `mcp__server__tool` names and permission checks. |
| `crates/hya-plugin` | Out-of-process plugin host. Owns the JSON-RPC stdio protocol, plugin client/host, manifest/config loading, command/tool dispatch, hook dispatcher bridge, permission bridge, and plugin-backed tool adapter. |
| `crates/hya-plugin-compat` | Compat plugin compatibility. The Rust crate exports shared `COMPAT_PLUGIN_VERSION` and `COMPAT_SDK_VERSION` constants; it does not resolve npm dependencies. The Bun adapter's `package.json` and `bun.lock` resolve and pin the supported Compat packages, discover Compat plugin config, load plugins, translate hook/tool/event methods, and expose the adapter runtime over JSON-RPC. |
| `crates/hya-plugin-example` | Placeholder stub binary (`fn main() {}`); does **not** speak the plugin protocol. Reserved for a future deterministic native-plugin QA fixture. For a real ABI reference, see `docs/plugin-protocol.md`. |
| `crates/xtask` | Dev-tooling entry point with working tasks: `sync-compat`, `migrate`, `startup-bench`, `matrix-check`, `package-bundle`, and `release-rehearsal`. |
| `crates/hya-e2e` | Process-level agent E2E harness (Track P): real `hya-backend` + FakeLlm. Matrix in `matrix.toml`; docs under `docs/testing/`. |
| `.planning` | Local task plans, findings, and progress using `planning-with-files`; existing tasks remain separate. |
| `docs/spec` | Project coding guidelines. Read the relevant layer's `index.md` before changing code. |
| `docs/development-history` | Preserved task artifacts and developer journals for historical reference. |
| `docs` | Project documentation: user guides, architecture, the `hya.v1` protocol references, historical Compat parity record, and testing/agent matrix under `docs/testing/`. |
| `DESIGN.md` | TUI design system: terminal-first visual rules, theme tokens, layout, transcript/input/overlay behavior. Read before touching TUI rendering. |

## Change Guidance

- Rust workspace uses edition 2024 and `rust-version = "1.91"`.
- Library crates deny `unwrap_used` and `expect_used`; keep panic paths out of
  library code and use typed errors where the crate already has one.
- Preserve the event-sourced architecture: append events, replay with the shared
  projection, and avoid parallel read-model logic that can drift from replay.
- Keep `hya-proto` free of heavy runtime dependencies.
- Put all new interactive terminal UI behavior in `packages/hya-tui-ts`. Do not
  reintroduce a Rust TUI crate, ratatui frontend, or backend-owned terminal
  renderer; the TypeScript TUI is the sole interactive frontend.
- Prefer existing planes (`PermissionPlane`, `InteractionPlane`, `SpawnerPlane`,
  `TodoPlane`, `SkillPlane`, `WebSearchPlane`, `LspPlane`) over adding another
  cross-cutting runtime channel.
- For TypeScript adapter work, keep it under
  `crates/hya-plugin-compat/adapter` and use the existing Bun/TypeScript
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
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```

Matrix and harness docs: `docs/testing/README.md`, `docs/testing/agent-matrix.md`,
`docs/testing/process-e2e.md`, `crates/hya-e2e/matrix.toml`.

For Compat adapter changes, also run from
`crates/hya-plugin-compat/adapter`:

```sh
bun run typecheck
bun test
```


For Workflow TUI presentation changes, also run from `packages/hya-tui-ts`:

```sh
bun test test/workflow-presentation.test.ts test/workflow-sidebar.test.ts
```

For TypeScript TUI changes, also run from `packages/hya-tui-ts`:

```sh
bun run typecheck
bun test
```

The old TUI's real-backend Track T trio
(`test/real-backend.test.ts`, `test/task-presentation.test.ts`,
`test/real-backend-agents.test.ts`) verified the deleted Compat surface and has
been removed; frontend-on-`hya-sdk-v1` coverage returns with the new TUI.
