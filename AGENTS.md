# Task Management

Use file-based planning for multi-step work. From the repository root, run
`python3 scripts/new-plan.py` to copy the [plan template](docs/templates/plan.md)
into the next numbered directory (`.plan/p1/`, `.plan/p2/`, …).
Resume an existing plan by its ID. Leave other plan directories intact.

- Keep the task breakdown and Kanban in `task_plan.md`. Each row has a stable
  ID, dependencies, an acceptance check, and one status. Update the row when
  starting work, encountering a blocker, or verifying completion.
- Record decisions, findings, and verification evidence in the plan. Use
  `findings.md` and `progress.md` alongside it if longer notes need their own
  files. Use the planning-with-files skill when available.
- Keep local development material in `.dev/`. Put local error-correction
  procedures, reproducers, and checks beyond unit tests in `.lint/`.
  Unit tests and shared CI gates stay with the components that own them.
- All three directories are local to the checkout and ignored by Git.
- Before editing a layer, read its guideline index under `.agents/spec/backend/`;
  consult `.agents/spec/guides/index.md` for cross-layer changes and code-reuse
  decisions.

## Forum Rule

- `.plan/` holds one task's working state; `.forum/` (conventions: global
  `AGENTS.md` and the `forum` skill) holds what other agents and later sessions
  need. When a finished plan produces something worth keeping (a pitfall, an
  environment fact, a decision and its reasons), record it on the forum as a
  question with its answer; put proposals and handoffs between agents in a
  discussion thread.

## Commit Rule

- When the user explicitly asks for commits, create one git commit per atomic change before reporting done.
- Stage only the files for that atomic change; never sweep in unrelated workspace changes.
- Use one-line semantic commit messages with no agent or AI attribution.
- Run applicable checks when possible and report failed or unavailable checks. Verification limits do not block a user-requested commit or push.

## Feature Workflow Rule

- Add focused regression coverage where useful and run the checks available for the touched area. TDD is a recommended approach, not a prerequisite for committing.
- Commit and push the atomic feature change when authorized by the user.
- If verification is unavailable or fails, report the commands and limitations without withholding an explicitly requested commit or push.

## Feature Documentation Rule

- Every new or modified feature must ship with its documentation in the same atomic change; a feature is not done until its docs exist.
- The documentation must cover three parts: an introduction (what the feature does and why it exists), usage (how to invoke or configure it — CLI commands and flags, config keys, TUI keys or slash commands, and a short worked example), and interface definitions (the exact contracts it exposes — HTTP/RPC routes with request/response schemas, event and payload types, tool names and parameter schemas, or config field names and types).
- Place the documentation using the structure in `docs/README.md` (`docs/guide/` + `docs/reference/` for users, `docs/extend/` + `docs/protocol/` for extension/API developers, `docs/architecture/` + `docs/contributing/` for core contributors; see `docs/contributing/development.md`); a genuinely new surface gets a new page under `docs/` linked from `docs/README.md` and `docs/site/astro.config.mjs`.
- Documentation is part of the feature's verification gate: do not commit or push feature work until the matching documentation is updated.

## TUI Preview & Terminal Test Rule

The TUI and the WebUI are one frontend: `packages/hya-tui-web` runs the TUI on
a real PTY and renders it in the browser with xterm.js (see `docs/architecture/server-relay-and-tui.md`,
ADR-0018). TUI verification uses a two-tier pyramid:

1. **In-memory UI suites (`packages/hya-tui/test/`)** — `@opentui/solid`
   `testRender` tests (`test/support/appHarness.tsx`, `test/ui-*.test.tsx`) run
   under `bun test` with no `hya serve` or PTY. They own UI layout,
   keybindings, overlays, views, composer/Vim, transcript, sessions, and
   subagent panes.
2. **Thin real-PTY / real-backend seam gate (`packages/hya-tui-web/tui-exp`)** —
   drives the actual TUI on a Bun PTY with `@xterm/headless` using shared
   `packages/hya-tui-web/e2e/*.spec.ts` scenarios strictly for OS, terminal
   protocol (OSC 52/9/0, `COLORFGBG`, bracketed paste), bare launcher, daemon
   lifecycle, gRPC, relay, and bundle VM seams. Browser-only cases are excluded
   from normal CI; Chromium remains available for optional WebUI checks and
   visual previews. Neither driver uses tmux or terminal scraping.

- **Preview in the browser, not a terminal multiplexer.** Do not use tmux,
  `script`, or terminal scraping to check TUI output. To look at the TUI, serve
  it and open the printed URL, for example:
  `HYA_BIN=target/debug/hya bun packages/hya-tui-web/src/main.ts --port 7681 -- bun packages/hya-tui/src/main.ts --dir "$PWD"`
  (the TUI attaches to the database's backend daemon, starting one if none
  runs; stop it with `target/debug/hya serve stop --db <db>` when done), or add
  `--server http://127.0.0.1:8080` to the TUI command to use a
  `hya serve --bind 127.0.0.1:8080` you run yourself. To preview bare `hya`
  itself (TUI + WebUI), make it the host command:
  `bun packages/hya-tui-web/src/main.ts --port 7681 -- target/debug/hya --port 0`.
  The offline echo model is enough; do not spend real provider calls on UI
  checks.
- **Add TUI coverage at the lowest tier that catches regressions.** Put UI,
  layout, keybinding, resize, and view behavior in `packages/hya-tui/test/`
  using `@opentui/solid` `testRender` (`test/support/appHarness.tsx`). Add or
  extend a shared PTY seam spec under `packages/hya-tui-web/e2e/` (using the
  `tui()` fixture from `e2e/harness.ts`) only for real-PTY, terminal-protocol,
  launcher/daemon, transport, or bundle/VM seams.
- **Assert on the terminal buffer, not pixels.** Use `waitForText`, `find`,
  `cell` (color `#rrggbb`, glyph `width`, bold/inverse), and `size`. Do not
  commit pixel-baseline screenshots, because fonts differ between machines.
  Use `waitForText`/`expect.poll`, never fixed sleeps.
- **For optional browser visual checks, inspect the results.** Each browser test
  writes `test-results/<test>/final-screen.png` and `final-screen.txt`. Open
  the PNG (agents: read the image) at the default 1100×640 viewport. If the
  change depends on size, also check a narrow viewport (about 80 columns).
  If browser execution is blocked, report that limitation; it does not block
  a user-requested commit or push.
- **Keep keybindings browser-safe.** The WebUI runs inside a browser, which
  reserves some shortcuts (Ctrl/Cmd+W, T, N, L, Tab, and Ctrl+Tab). Do not bind
  core TUI actions only to these. Every binding must be reachable through
  `press()` in a spec.
- **Keep the host generic.** `packages/hya-tui-web` has no hya-specific logic
  and runs only the fixed command it was started with. The WebUI shows exactly
  what the TUI draws; a browser-only feature needs its own ADR. Rendering never
  moves into `hya serve`.
- **Frame contract.** The `/pty` WebSocket speaks the protojson
  `hya.v2` `PtyClientFrame`/`PtyServerFrame` shapes. A change to that contract
  updates `src/frames.ts`, its unit tests, and `docs/architecture/server-relay-and-tui.md` together.

## Release & Changelog Rule

- Backend and frontend share a single unified release version. `versions.toml` is the aggregate source (`version = "<semver>"` and `minimum_backend_version = "<semver>"`).
- A release updates `versions.toml` (`version`), `[workspace.package].version`, the `hya-backend` package version, `packages/hya-tui/frontend-version.ts` (`frontendVersion`), `README.md`, `CHANGELOG.md`, and uses a canonical `v<version>` Git tag (`vx.x.x` or `vx.x.x-<prerelease>` with optional suffix such as `v0.47.0` or `v0.47.0-dev.1`, strictly matching `^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$`). Non-version or split tags (`backend/*`, `frontend/*`, `phase*`, bare versions without `v`, or `+` build metadata) MUST NOT exist locally or on `origin`; when retracting a published release tag, use `gh release delete <tag> --cleanup-tag --yes` (and `git tag -d <tag>`) so no orphaned `untagged-*` draft Release remains. `CHANGELOG.md`'s first heading MUST be exactly `# <version>` (no `v` prefix).
- Each target publishes one unified archive `hya-<version>-<target>.tar.gz` containing `bin/hya`, `bin/hya-migration`, the fourteen first-party bundles, the Argus example bundle, Bun, `lib/hya/code-mode`, `lib/hya/bun-adapter`, `lib/hya/claude-adapter`, `lib/hya/tui`, `lib/hya/tui-web`, and `lib/hya/tui-sdk`; releases never carry an installer script.
- The single installer is `scripts/hya-install.sh`, served from `https://hya.ed-aisys.com/install.sh` on `main` and embedded in `hya update`. It downloads `releases/latest/download/SHA256SUMS` by default (or `releases/download/v<version>/SHA256SUMS` when `--version` is given; `--version` accepts and normalizes an optional leading `v`), verifies the archive's SHA-256 digest, swaps the prefix in by rename, and rolls back on failure.
- Release publication is tag-driven by `.github/workflows/release.yml`: push `v<version>` (tags containing `-` such as `v0.47.0-dev.1` publish as GitHub Pre-releases). Main-branch pushes, the schedule, and `workflow_dispatch` refresh dependency caches and MUST NOT publish a release.
- Before pushing a release tag, run `cargo test -p xtask -p hya-bundle`, `actionlint .github/workflows/release.yml` (when installed), `bash tests/hya_install_script.sh`, and `release-rehearsal --no-publish` on a supported target. Push the version commit before the tag.
- The packaged frontend declares `minimum_backend_version` in `versions.toml` and `minimumBackendVersion` in `packages/hya-tui/frontend-version.ts` (for attaching to older daemons or remote backends). It accepts only backend versions greater than or equal to that minimum; missing, malformed, or older backend versions MUST be rejected during bootstrap, backend switching, and remote entry. Raise the minimum only when the frontend requires a newer backend contract.
- Every shipped behavior change updates the unified aggregate version once. Shipped behavior includes Rust crates, `proto/`, `bundles/`, `packages/hya-tui`, `packages/hya-tui-web`, the Bun adapter, and anything else in the release archive or source install.
- Documentation-only changes (`docs/`, `*.md` files, code comments, `AGENTS.md`, `.plan/`), CI-only changes (`.github/`, CI scripts and config), and test-only changes that leave shipped code untouched MUST NOT bump the aggregate version or write a new changelog.
- Only the root aggregate version is a real release version (`@hya/tui-sdk` maintains its independent SDK version). Other Rust package manifests use `0.0.0` plus `[package.metadata.hya] version-reference = "hya"`; frontend and adapter package manifests use `0.0.0` plus `"hya": { "version-reference": "hya" }`. Bundle source manifests use identity version `0.0.0` plus `version_ref: hya` (legacy `backend` is also accepted); preparation resolves that reference to `hya_version::VERSION`.
- `CHANGELOG.md` contains only the newest release notes. When the version advances, archive its previous file under `docs/changes/CHANGELOG_<version>.md`; historical `CHANGELOG_BACKEND_*.md` and `CHANGELOG_FRONTEND_*.md` files under `docs/changes/` remain untouched as archive history.
- Version checks: `cargo test -p xtask` validates unified aggregate/reference metadata, `CHANGELOG.md` heading, tag grammar, frontend minimum-backend compatibility, and release layout; `cargo test -p hya-bundle` validates bundle preparation and resolved identities; release rehearsal validates `README.md`, `Cargo.lock`, and packaged assets.

## Project Overview

`hya` is a modular Rust multi-agent coding agent (ADR-0039). It is built as an
event-sourced workspace: user prompts, model deltas, tool calls, permissions,
token usage, session lifecycle changes, and bundle-owned domain state
(`Event::Extension`) are appended as `Event`s to SQLite v2 (`format_version = 2`),
then replayed into a deterministic `Projection` (with bundle `ProjectionReducer`s
populating `Projection::extensions`). The core binary is `hya-bare` (run standalone
with global `--bare` or `--bare --pure`); enabled bundles from `bundles/builtin/`
(14 first-party bundles over Lockstep Native ABI v2, static presets, and
`@hya/tui-sdk` v2) compose `hya-full` by default at runtime.

The interactive frontend is the Bun/OpenTUI TUI in `packages/hya-tui` (v2
HTTP/JSON+SSE or direct gRPC client, modular core panes + bundle-contributed
`@hya/tui-sdk` v2 panes); `packages/hya-tui-web` renders the same TUI in a
browser as the WebUI. Bare `hya` on a terminal starts both against the database's
backend daemon (found or auto-started; it outlives its clients, ADR-0023).
`hya-sdk` and gRPC are the other supported ways to drive a backend.

The server exposes exactly one contract — `hya.v2` (17 services / 104 rpcs in
`proto/hya/v2`) — over HTTP/JSON+SSE+WebSocket under `/v2` and over gRPC
through `hya_server::V2Grpc`, which dispatches through the same router. Both
are served on the same port from one server state (`hya_server::build`,
routed by `content-type: application/grpc*`), also through the relay;
`HYA_GRPC_BIND` only adds an optional extra listener. Domain endpoints for both
Native ABI v2 (`NativeBundleApiHandler`) and JS bundles mount uniformly via
`BundleApi`, and domain events stream via `ExtensionEvent`. Legacy v1 contracts
are removed; offline upgrades from v1 are handled exclusively by the standalone
`hya-migration` binary (`crates/hya-migration`).

The main runtime path is:

```text
hya / hya-server (/v2 HTTP+SSE+WS & V2Grpc + BundleApi)
  -> hya-app config v2 / auth / plugin / MCP / code-mode composition & HostRuntime
  -> hya-engine::SessionEngine (on the hya-core 3-primitive kernel)
  <-> bundles/builtin/* (10 Native ABI v2 cdylibs via hya-runtime-abi, 3 static, 1 TUI v2)
  -> hya-provider streaming model route
  -> hya-tool slim ToolCtx + ExtensionPlanes (builtin, MCP, or plugin tools)
  -> hya-store SQLite v2 event log (event_log, bundle_state, bundle_claim)
  -> v2 clients (hya-sdk, gRPC, packages/hya-tui) over the same projection
```

Workflow authoring and normalization live in `hya-workflow`; durable Workflow
execution, Goal mode (`-p`), Loop mode (`hya loop`), episodic subagents, and
vertical channels live in their respective Native ABI v2 bundles under
`bundles/builtin/` (`workflow`, `goal`, `loop`, `multi-agent`, `agent-channels`).
Stop decisions are owned by drivers and separate evaluators/verifiers; workers
do not decide that their own objective is done.

## Component Map

| Component | Feature |
| --- | --- |
| `crates/hya-backend` | Package for the unified `hya` executable — the primary terminal entry point; subcommands select the controlled area. Supports global `--bare` (`BuiltinBundleProfile::Bare`) and `--pure` across interactive `hya`, `serve`, `serve start|restart`, `exec`, `acp`, `agent list`, `-p`, `loop`, and `workflow`. Bare `hya` on a terminal finds or starts the database's detached backend daemon (`src/daemon.rs`, `hya serve start|status|stop|restart`; `--backend <url>` to use a given one) and starts the Bun TUI and the WebUI host as child processes (`src/frontend.rs`: asset/Bun resolution, lifecycle, log file; `--port`, default 3250; see `docs/reference/cli.md` and `docs/guide/daemon-and-relay.md`, ADR-0020, ADR-0023); without a terminal it prints a guidance banner. Subcommands cover `acp` (editor stdio, see `docs/guide/editor-integration.md`), `exec`, `-p/--prompt` goal mode (`hya/goal:goal` driver), `loop` (`hya/loop:loop` driver), `serve` (with `--relay` and `serve relay connect|disconnect|status|link|rotate`), `tail-session`, `sessions`, `login`/`oauth`/`auth`, `agent`, `bundle`, `workflow` (routed via `hya/workflow` `BundleApi`), `models`, `update`, and the secure relay's `proxy`, `bridge`, and `relay doctor` (`proxy_cmd.rs`, `bridge.rs`, `relay_doctor.rs`, `serve_relay.rs`; bare `hya --connect <link>` runs the bridge in-process instead of a daemon; see `docs/guide/daemon-and-relay.md`). Runtime commands **compose** through `hya-app`. Build with `cargo build -p hya-backend --bin hya`. |
| `crates/hya-app` | Runtime composition library (not a binary). `config.yaml` (`schema_version: 2`) and per-bundle `<config-dir>/bundles/<percent-encoded-id>/config.yml` loading, `BuiltinBundleSelection` (`bundles.builtin.{profile,enable,disable}`), core Code Mode integration (`code_mode.rs` + `packages/hya-code-mode`), `HostRuntime` implementation (`host_runtime.rs`), provider/auth resolution, MCP and plugin wiring, permission policy construction, session engine build, and installed-bundle catalog refresh. Also the per-Project bundle and plugin tiers (`project_bundles.rs`, `project_scope.rs`, `project_plugins.rs`): `.hya/bundles`/`.hya/plugins` of every root of a registered Project, first root wins, hot-respawned on a manifest edit (ADR-0027) — not read from the process's own directory. Prefer this crate over `hya-backend` when changing composition, not CLI surface. |
| `crates/hya-bundle` | `hyabundle` `schema_version: 2`-only prepare/validate/catalog types, `BuiltinBundleProfile`/`BuiltinBundleSelection`, `overrides: hya/bare` resolution, and package fixtures. Catalog builders and resource/agent/Workflow resolution used by install CLI and process E2E. Also the runtime loader for the fourteen trusted built-in bundles under `bundles/builtin/` (`first_party_bundle`; see `docs/extend/hyabundle-spec.md` and `docs/extend/bundles.md`). Prefer this crate for bundle authoring contracts and prepare semantics. |
| `bundles/builtin` | Built-in first-party bundles (14 total, ADR-0039): ten Native ABI v2 `cdylib` bundles (`hashline`, `coding-extras`, `compaction`, `network`, `plan`, `goal`, `loop`, `workflow`, `agent-channels`, `multi-agent` — of which `plan`, `workflow`, `agent-channels`, and `multi-agent` are hybrid `native` + `@hya/tui-sdk` v2 bundles), three static preset bundles (`core-agents`, `core-skills`, `core-commands`), and one standalone TUI v2 bundle (`basic-tui-components`). Native `cdylib` crates obey the zero-linkage red line (never linking `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, or `tonic`). Verified independently by `.github/workflows/builtin-bundles.yml` (`bundles/extra/` and `bundles/examples/` are verified independently by `.github/workflows/bundles.yml`; see `docs/extend/hyabundle-spec.md`). |
| `crates/hya-runtime-abi` | Lockstep Native Runtime ABI v2 (`export_native_bundle_v2!`, `abi_digest_v2`, `NativePluginContext`, `NativeRegistrations`, `Arc<dyn HostRuntime>` and its seven sub-traits: `HostSessionStore`, `HostBundleStorage`, `HostTurnRunner`, `HostModelCaller`, `HostEventBus`, `HostCatalogReader`, `HostExtensionRegistry`; plus `NativeService`, `NativeHook`, `NativeBundleApiHandler`, `ProjectionReducer`, `UriSchemaHandler`, `NativeDriver`). Zero dependency on `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, or `tonic`. See `docs/extend/native-abi.md`. |
| `crates/hya-workflow` | Workflow source parsing, normalization, validation, and immutable compiled plans (`CompiledWorkflow`). Prefer this crate for authoring/compile contracts; durable execution lives in `bundles/builtin/workflow`. |
| `crates/hya-core` | The three-primitive kernel (`kernel`: model append, tool append, rewrite) with seven standard kernel tools (`read`, `write`, `edit`, `bash`, `grep`, `todo`, `rewrite`) plus built-in `skill`, permanently reserved `todo`/`rewrite` invariants, `HandleRouter` integration, kernel prompt composition (ADR-0035), the internal native bundle loader (`bundles::load_internal_with_runtime`), and shared `CoreError`. Calls `hya-tool-loader` to load internal native libraries and uses `hya-context` for rewriting and token accounting. |
| `crates/hya-engine` | Slimmed single-agent `SessionEngine` + extension host running on the `hya-core` kernel (`SessionEngine::new_bare` for `--bare` with `BARE_AGENT_ID = "hya-bare"`). Owns turns, turn admission, streaming rounds, shell turns, event bus, prompt construction, native/JS hook dispatch, `NativeService`/`ProjectionReducer` wiring, session forking, and the per-Project catalog scope tier (`catalog_scope.rs` `CatalogScope`/`ScopeOverlay`, `RuntimeRegistry` scope overlays with an LRU/TTL cache; ADR-0027). |
| `crates/hya-proto` | Shared wire/domain types. Defines newtyped IDs, tagged kernel `Event` + `Event::Extension` / `Envelope`, messages, parts, roles, model/tool schema types, API DTOs, and the deterministic `Projection` reducer (with `Projection::extensions`). Keep this dependency-light so UI/client crates can reuse it cheaply. |
| `crates/hya-provider` | Model provider abstraction. Normalizes OpenAI-compatible, OpenAI Responses, OpenAI Codex, Grok Build, Anthropic, Google, dev, and fake routes into one streamed `Event` model; handles protocol encoding/decoding, provider routing, capability metadata, reasoning effort, and preflight checks for tool-capable routes. |
| `crates/hya-tool` | Tool and permission plane. Provides the `Tool` trait, `ToolRegistry` (enforcing permanently reserved `todo`/`rewrite` and supporting `ns__tool` names), allow/ask/deny rules, the multi-root workspace boundary (`ProjectScope`: canonical containment in the session's roots, outside → `ExternalDirectory` ask; bash exempt; ADR-0026), `HandleRouter` (`DynamicUriHandler`/`UriSchemaPlane`), slim single-agent `ToolCtx` + lockstep-ABI-safe `ExtensionPlanes` (`ErasedPlane`, keyed by `type_name`), interaction/skill/nested planes, and native ABI runtime bridge helpers. |
| `crates/hya-tool-loader` | Host-side loader for Lockstep Native ABI v2 dynamic libraries (`load_v2`, `NativeBundleLoader`). Verifies the 32-byte `hya_native_abi_v2` digest, bridges Tokio runtime TLS (`hya_native_with_runtime_v2`) and run-deadline cell (`hya_native_share_deadline_v2`), and collects `NativeRegistrations`. |
| `crates/hya-tool-error` | Tool and permission error types shared by the native tool, rewrite, and ABI v2 contracts. |
| `crates/hya-context` | Token estimation and context-window accounting (`tokens`), and the native rewrite extension contract and host services (`rewrite`). |
| `crates/hya-store` | SQLite WAL v2 persistence (`format_version = 2`, `schema_meta`). Stores `event_log`, `projection_snapshot`, `project`/`project_root`, generic `bundle_state` (CAS KV), generic `bundle_claim` (fenced leases), `pending_resume`, `saved_permission`, and `file_blob`. Fails fast on unmigrated v1 databases directing to `hya-migration`. |
| `crates/hya-server` | The `/v2` contract surface over `hya-engine`: HTTP/JSON+SSE+WebSocket routes generated from `proto/hya/v2`, `V2Grpc` (tonic) dispatching through the same router, and unified `BundleApi` mounting both Native ABI v2 (`NativeBundleApiHandler`) and JS bundle routes. Shared catalogs/guidance/PTY/worktree/git helpers live in `support`. Also the Host allowlist guard (`host.rs`, `--allow-host`), the relay host connector (`relay_host/`, driven by loopback-only `RelayControl` rpcs), Projects and session placement (ADR-0024), and the reaper of unused ephemeral sessions (`ephemeral.rs`). |
| `crates/hya-api` | The v2 dual-protocol contract crate: `proto/hya/v2` generated Rust types (prost/tonic/pbjson protojson), stable error-code table mapped to HTTP statuses and gRPC codes, and cursor helpers. Regenerate with `cargo run -p xtask -- gen-api` (vendored protoc; output committed). |
| `crates/hya-sdk` | Typed Rust SDK for the v2 API (`Sdk`, `Client`): bootstrap, sessions, event-driven turns, transcript/todo reads, curated replay, interactions, `BundleApi`, live SSE `StreamFrame` subscription, and `SessionMirror` transcript + `extensions` folding. |
| `crates/hya-migration` | Standalone offline v1-to-v2 migration and SHA-256 rollback binary (`bin/hya-migration`, shipped alongside `bin/hya`): `check`, `run [--dry-run] [--backup-dir]`, and `rollback --backup <dir>` covering `sessions.db`, `config.yaml` + per-bundle `config.yml`, `tui.toml`/`tui.json`, `.hya/bundles/`, and `registry.sqlite3`. See `docs/guide/migration-v1-to-v2.md`. |
| `crates/hya-updater` | `hya update`'s release installer: embeds `scripts/hya-install.sh` and pipes it to `sh` for the running prefix (`release_install`), plus the command surface (`cli`: `version`). Must not depend on runtime crates. See `docs/getting-started/installation.md`. |
| `crates/hya-mcp` | MCP support. Implements the MCP protocol/client/manager and bridges MCP tools into `hya-tool` with namespaced `mcp__server__tool` names and permission checks. |
| `crates/hya-child-io` | Bounded NDJSON framing and connection task tracking shared by MCP and plugin stdio transports. |
| `crates/hya-plugin` | Out-of-process plugin host. Owns the JSON-RPC stdio protocol, plugin client/host, manifest/config loading, command/tool dispatch, hook dispatcher bridge, permission bridge, and plugin-backed tool adapter. |
| `crates/hya-plugin-bun` | Bun extension adapter (`kind: bun`). The Bun adapter under `adapter/` loads bundle JS extensions (`--bundle-extension`/`--extension`), translates hya wire hooks/tools/events, and exposes the runtime over NDJSON JSON-RPC stdio. |
| `crates/hya-plugin-claude` | Claude Code plugin adapter (`kind: claude`). The Rust crate decodes the source envelope for `hya bundle install --claude`. The Bun adapter under `adapter/` translates Claude plugin directories and marketplace entries into hya bundles and runs configured and bundled Claude command hooks over JSON-RPC stdio. Installed at `lib/hya/claude-adapter`. |
| `crates/hya-relay` | Secure relay (ADR-0025): the `hya.relay.v1` rendezvous protocol (`proto/hya/relay/v1`, separate from `hya.v2`; generated prost/tonic code committed, regenerate with `cargo run -p xtask -- gen-relay`), the binding-independent `RelayTransport` message stream, the `RelayLink` (`hya://…` link grammar, room-id derivation), relay identity keys, the Noise `NKpsk0` tunnel (`tunnel.rs`), the proxy core (`proxy/`: rooms, host registration, splicing, limits), and the gRPC + WebSocket bindings on both sides (`server/`: one listener, path prefix, TLS; `client/`: `t=auto` negotiation, heartbeat, reconnect). Must not depend on hya runtime crates; the CLI surfaces (`hya proxy`, `hya relay doctor`, `hya bridge`, `hya serve --relay`) live in `hya-backend`, the host connector in `hya-server::relay_host`. See `docs/guide/daemon-and-relay.md` and `docs/architecture/server-relay-and-tui.md`. |
| `crates/hya-version` | Compile-time release and minimum compatible backend versions from `versions.toml`. |
| `crates/xtask` | Dev-tooling entry point with working tasks: `boundary-check` (ADR-0040 core/bundle guard, `boundary.toml`; also run by `cargo test -p xtask`), `startup-bench`, `matrix-check`, `package-bundle`, `release-rehearsal`, `gen-api`, and `gen-relay`. |
| `crates/hya-e2e` | Process-level agent E2E harness (Track P): real `hya` + FakeLlm. Matrix in `matrix.toml`; docs in `docs/contributing/testing.md`. |
| `packages/hya-code-mode` | Core Code Mode JS runtime (promoted from a built-in bundle into `hya-bare`, ADR-0039; wired by `crates/hya-app/src/code_mode.rs` and shipped as `lib/hya/code-mode`). Exposes `tools.*` and `workspace.state` by default and dynamically omits `agent()` when `hya/multi-agent` is not loaded. |
| `packages/hya-tui` | Bun/OpenTUI TUI over the `hya.v2` HTTP/JSON+SSE or direct gRPC contract: five core panes (`conversation`, `composer`, `activity`, `status`, `layout`), generic `extension` pane dispatch for `@hya/tui-sdk` v2 bundles (`hya/basic-tui-components`, `hya/multi-agent`, `hya/workflow`, `hya/plan`, `hya/agent-channels`), v4-only `paneLayout` persistence, zero-bundle `--bare` startup, the Provider View (`/key`), Projects (`/project`, `/new --temp`), remote backends (`--remote`, `/connect-remote` through a `hya bridge` child), slash completion, and a generic `/api` command view. Started by bare `hya` (`--server <daemon URL> --dir <cwd> --db <db> --hya <hya>`, or with `--connect` `--server <bridge URL> --hya <hya> --remote --server-label …`; plus `--web-url|--web-error`); direct starts may pass `--grpc <host:port>`. Shipped as `lib/hya/tui` in the release archive and source install, so it must stay self-contained (`src/operations.json` and `src/proto/hya/v2` are generated by `gen-api`). Also runs directly with Bun for development (`bun packages/hya-tui/src/main.ts`). See `docs/guide/interactive-mode.md` and `docs/architecture/server-relay-and-tui.md`. |
| `packages/hya-tui-sdk` | Standalone `@hya/tui-sdk` v2 (`api_version: 2`, `sdk: "2.0.0"`) authoring package and sandboxed QuickJS WASM / trusted JSC extension host. Provides `tui.bundle_api`, `tui.extension_state.read`, `tui.transcript.embed` (`{ type: "transcript", sessionId }`), dynamic named panes (`api.registerPane`), and slash commands (`api.registerCommand`). Shipped as `lib/hya/tui-sdk`. See `docs/extend/tui-extensions.md`. |
| `packages/hya-tui-web` | Bun host that runs a terminal frontend on a real PTY and renders it in the browser with xterm.js (WebSocket frames reuse `hya.v2` `PtyClientFrame`/`PtyServerFrame`). Playwright harness for TUI visual/interaction tests and the WebUI host that bare `hya` starts (shipped as `lib/hya/tui-web`). See `docs/architecture/server-relay-and-tui.md`. |
| `public` | Static GitHub Pages root landing page: `index.html`, `style.css`, `main.js`, and `assets/`. Plain HTML/CSS/JS with no build step; assembled at `/` of the GitHub Pages artifact with the Starlight documentation site mounted under `/docs/` (`docs/site/scripts/assemble-pages.mjs`). Not part of the release archive, so changes need no version bump. |
| `.plan/pN/` | Numbered local plans and their Kanban task tables. |
| `.dev/` | Local development notes, experiments, and helpers. |
| `.lint/` | Local error-correction material and checks beyond unit tests. |
| `.agents/spec` | Project coding guidelines. Read the relevant layer's `index.md` before changing code. |
| `docs` | Project documentation organized into Getting Started (`docs/getting-started/`), Guides (`docs/guide/`), Reference (`docs/reference/`), Extending & Protocol (`docs/extend/`, `docs/protocol/`), and Architecture & Contributing (`docs/architecture/`, `docs/contributing/`, `docs/adr/`). |

## Change Guidance

- Rust workspace uses edition 2024 and `rust-version = "1.91"`.
- Library crates deny `unwrap_used` and `expect_used`; keep panic paths out of
  library code and use typed errors where the crate already has one.
- Preserve the event-sourced architecture: append events (or `Event::Extension`),
  replay with the shared `Projection` + bundle `ProjectionReducer`s, and avoid
  parallel read-model logic that can drift from replay.
- Keep `hya-proto` free of heavy runtime dependencies, and enforce the
  zero-linkage red line for every native `cdylib` bundle under `bundles/builtin/`
  (never link `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, or `tonic`;
  mediate exclusively through `Arc<dyn HostRuntime>`).
- TUI work goes into `packages/hya-tui` (ADR-0019) or built-in `@hya/tui-sdk` v2
  bundles under `bundles/builtin/*/tui/`. Do not add another interactive frontend
  (a Rust TUI crate, ratatui frontend, or second TypeScript terminal frontend) or
  move rendering into the backend without an explicit decision. Bare `hya` only
  orchestrates the Bun processes (ADR-0020); it never renders. The TUI reads the
  server projection over the v2 contract; it does not build a competing durable
  state model from SSE deltas.
- Prefer core planes (`PermissionPlane`, `InteractionPlane`, `SkillPlane`,
  `HandleRouter`/`UriSchemaPlane`, `ExtensionPlanes`) and `Arc<dyn HostRuntime>`
  over adding another cross-cutting runtime channel.
- For TypeScript adapter work, keep it under
  `crates/hya-plugin-bun/adapter` and use the existing Bun/TypeScript
  scripts instead of adding another JS toolchain.

## Verification

- After any fix, feature, or refactor, run the checks for the affected components and build a local executable before reporting done. Documentation-only changes need no build or tests; CI-only changes need only a syntax check of the edited workflow.
- **Verify by component, not the full suite.** Local development runs only the tests of the components a change touches, plus the components that consume a changed public contract. CI (`.github/workflows/ci.yml`) runs the full workspace suite, Track P, and all TUI checks on every push; do not repeat that locally.
- **CI gate & test ownership.** `.github/workflows/ci.yml` uses `cargo-nextest` (with `.config/nextest.toml` slow-test guards: warn 5s / terminate 20s in `ci`, warn 15s / terminate 60s in `ci-e2e`), Rust's bundled `rust-lld` linker, and a shared `build-bin` job whose `hya`/`xtask` artifacts feed `e2e` and `tui`. Bundle verification is split into two independent workflows: `.github/workflows/builtin-bundles.yml` for `bundles/builtin/**` and `.github/workflows/bundles.yml` (backed by `scripts/bundle-ci/`) for `bundles/extra/**` and `bundles/examples/**` (ADR-0038). Enforce single-layer test ownership — deep engine/API semantics belong in Track I, UI behavior belongs in `packages/hya-tui/test`, and real-process seams belong in Track P or `packages/hya-tui-web/e2e`. Keep PR tests free of real sleeps > 1s (use injectable clocks/deadlines or event-driven waits; mark heavy OS-fault/soak tests `#[ignore]` for `.github/workflows/nightly.yml`).
- Do not run `cargo test --workspace`, `cargo clippy --workspace`, the full `hya-e2e` matrix, or the full Playwright suite locally unless the user asks, or the change is a cross-cutting refactor that touches most crates (for example a workspace-wide dependency or lint bump).
- Report which components you verified and why that scope is enough. If you skipped a consumer on purpose, name it.

### Choosing the affected components

- A change private to one crate or package (internal functions, tests, non-`pub` items): test that crate or package only.
- A change to a crate's public API, wire types, or behavior that other crates rely on: also test its direct workspace dependents. List them with:

```sh
cargo tree --workspace -i <crate> --depth 1 -e normal,dev --prefix none
```

- Foundation crates (`hya-proto`, `hya-api`, `hya-tool`) have many dependents. Test the changed crate, then only the dependents that use the changed items (find them with `grep` or `xd://lsp` references), not the whole list.
- `proto/hya/v2` changes: regenerate with `cargo run -p xtask -- gen-api`, then test `hya-api`, `hya-server`, `hya-sdk`, and `packages/hya-tui` when its generated catalog or protobuf definitions change.
- Version bumps: `cargo test -p xtask` (frontend `package.json`) and `cargo test -p hya-bundle` (bundle identities) check the coupled versions; `README.md` and `Cargo.lock` are checked only by `release-rehearsal`.
- `crates/hya-e2e/matrix.toml` or new Track P tests: `cargo run -p xtask -- matrix-check`.

### Rust components

```sh
cargo fmt --all --check
cargo clippy -p <crate> [-p <dependent> ...] --all-targets -- -D warnings
cargo test -p <crate> [-p <dependent> ...]
cargo build -p hya-backend --bin hya
```

`cargo fmt --all --check` is cheap and stays workspace-wide. Pass `-p` once per affected crate so one cargo run covers all of them. Run one test module or filter (`cargo test -p <crate> --test it <module>::` or `cargo test -p <crate> <filter>`) while iterating; run the affected crates' full tests before reporting done.

### Process agent E2E (`crates/hya-e2e`)

Run Track P only when a change touches `crates/hya-e2e` or an agent surface the PR matrix covers (permissions, skills, MCP, subagents, hyabundle). Track P is organized into 11 consolidated golden-path modules safe for parallel execution; run the module(s) for the surface you changed, not the whole matrix:

```sh
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e --test it <pNN_module>:: -- --test-threads=4
```

Track P stays out of any workspace-wide `cargo test` (`--exclude hya-e2e`) and runs in CI with `cargo nextest run --profile ci-e2e -p hya-e2e` (`--test-threads=4`). Heavy real-OS storage fault/soak tests (`p43_store_robustness.rs`) are marked `#[ignore = "nightly OS fault/soak"]` and run serially in `.github/workflows/nightly.yml` (`--ignored --test-threads=1`). Matrix and harness docs: `docs/contributing/testing.md`, `crates/hya-e2e/matrix.toml`.

### Bun components

Run only the packages you changed, from that package's directory:

|Changed package|Commands|
|---|---|
|`crates/hya-plugin-bun/adapter`|`bun run typecheck && bun test`|
|`crates/hya-plugin-claude/adapter`|`bun run typecheck && bun test`|
|`packages/hya-tui-sdk`|`bun run typecheck && bun test`|
|`packages/hya-tui`|`bun run typecheck && bun test` (unit + `@opentui/solid` `testRender` UI suites; use `bun test <file>` while iterating)|
|`packages/hya-tui-web`|`bun run typecheck && bun test ./test`|

A TUI change touching PTY, terminal protocol, launcher/daemon, gRPC/relay transport, or bundle/VM seams also runs the matching shared terminal seam specs from `packages/hya-tui-web` (`bun run test:tui-exp:parity -- e2e/<spec>.ts`), not the whole suite. Specs that start the backend or bundle packager need `cargo build -p hya-backend --bin hya -p xtask --bin xtask` first (see `docs/contributing/testing.md`).
