# Design Philosophy & Architecture Overview

`hya` is a Rust multi-agent coding runtime built around a single architectural
commitment: **every state transition in a coding session — user prompts, model
stream deltas, tool executions, permission decisions, token usage, subagent
episodes, inter-agent mail, and Workflow stage transitions — is captured in a
canonical, append-only `Event` stream and folded deterministically through one
shared `Projection` reducer.**

For visual system topology, crate dependency graphs, sequence flows, and state
machines, see [Architecture & Design Diagrams](diagrams.md).

---

## Core Design Philosophy

### 1. Event Sourcing Without Parallel Read Models

The SQLite `event_log` in `hya-store` and the pure `Projection` reducer in
`hya-proto` are the sole durable source of truth:

- **Unified live and replay envelopes:** Every event travels wrapped in an
  `Envelope { seq, ts_millis, event }`. High-frequency assistant text deltas
  stream live at `seq = 0` without touching disk; at each part boundary, a
  single transaction commits the durable `TextStart` / `TextReplace` / `TextEnd`
  batch (`seq > 0`).
- **Idempotent deterministic folding:** `Projection::apply` ignores duplicate
  or older durable sequence numbers (`seq <= last_seq`), allowing store reads,
  `projection_snapshot` caches, SSE reconnects, `hya-sdk-v1`'s
  `V1SessionMirror`, and `hya tail-session` to share identical semantics without
  state drift.

### 2. Persistent Daemon & Client-Chosen Multi-Root Workspaces

`hya` separates **where the runtime lives** from **where a session works**:

- **One writer per database, persistent daemon ([ADR-0022](../adr/0022-one-writer-per-database.md), [ADR-0023](../adr/0023-persistent-backend-daemon.md)):**
  A SQLite session database (`$XDG_STATE_HOME/hya/sessions.db` by default) is
  owned by at most one server process via kernel `flock`
  (`<db>.runtime-owner.lock`) and advertised through `<db>.server.json`. Bare
  `hya` discovers or auto-starts a detached daemon that outlives every terminal
  and browser client, making subsequent launches instant and allowing multiple
  TUIs, WebUI tabs, and CLI invocations (`hya exec --db`, `hya sessions`) to
  collaborate live on the same database.
- **No server `cwd` — client-chosen `Project`s ([ADR-0024](../adr/0024-project-model-and-client-chosen-workspace.md), [ADR-0026](../adr/0026-multi-root-path-permissions.md)):**
  `hya serve` has no working directory of its own. Clients bind sessions either
  to a named **Project** (one or more ordered absolute directory roots on the
  backend host) or to an isolated **Temporary session**
  (`$XDG_CACHE_HOME/hya/scratch/<session_id>`). File tools check canonical,
  symlink-resolved containment against all roots of the session's Project via
  `ProjectScope`; paths outside require explicit `ExternalDirectory` approval.
- **Lazy per-Project `CatalogScope` ([ADR-0027](../adr/0027-per-project-catalog-tier.md)):**
  Instead of one process-wide directory snapshot, `RuntimeRegistry` composes
  lazy LRU/TTL `ScopeOverlay`s per `Global`, `Directory`, or `Project` scope —
  loading `<root>/.hya/bundles`, `<root>/.hya/plugins`, `.hya/skills`,
  `.hya/commands`, and `AGENTS.md` across Project roots (first root wins) and
  hot-reloading on manifest changes.

### 3. One Consolidated Dual-Protocol Contract (`hya.v1`) + End-to-End Encrypted Relay

Every client drives the backend over one contract defined in `proto/hya/v1`:

- **18 services / 107 RPCs on a single port:** `hya_server::build` serves both
  HTTP/JSON + SSE + WebSocket (`/v1/*`) and tonic gRPC (`V1Grpc`, routed by
  `content-type: application/grpc*`) from the same `AppState`. Unary gRPC calls
  dispatch in-process through the same Axum `/v1` router, guaranteeing
  HTTP/gRPC behavioral and error-code parity by construction.
- **Zero-trust remote access ([ADR-0025](../adr/0025-secure-relay.md)):**
  `hya proxy` runs a stateless blind rendezvous (`hya.relay.v1` over gRPC or
  WebSocket with `t=auto` negotiation). A backend joins a signed Room
  (`hya serve --relay`), and a remote client connects via `hya --connect` or
  `hya bridge`, tunneling raw `hya.v1` HTTP/SSE/WebSocket inside end-to-end
  `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s` streams. The relay sees ciphertext and
  room IDs only; loopback-only admin RPCs (`RelayControl`) are rejected on
  `Origin::Relay`.

### 4. One Frontend Across Terminal and Browser, Zero Rendering in Rust

- **Single interactive TUI ([ADR-0019](../adr/0019-adopt-opentui-frontend.md)):**
  `packages/hya-tui` (Bun + OpenTUI) is the sole interactive frontend, reading
  server projections over `hya.v1` HTTP/SSE or direct gRPC without maintaining a
  competing durable state model.
- **Real-PTY WebUI & test harness ([ADR-0018](../adr/0018-browser-rendered-tui-test-environment.md), [ADR-0020](../adr/0020-bundle-tui-and-webui-in-hya.md)):**
  `packages/hya-tui-web` runs the exact same TUI binary on a real PTY and
  streams `PtyClientFrame`/`PtyServerFrame` frames to xterm.js in the browser
  (default `http://127.0.0.1:3250`), forwarding OSC 0/2 tab titles and OSC 9/777
  desktop notifications. Bare `hya` only supervises child processes — rendering
  never enters the Rust backend.

### 5. Episodic Resident Subagents, Vertical Channels & Engine-Owned Stop Decisions

- **Unified episodic actor model ([ADR-0015](../adr/0015-unified-resident-subagent-lifecycle.md)):**
  Every spawned subagent is a non-blocking **resident actor** backed by its own
  event-sourced `Session` and fenced by a transactional `(SessionId, ActorEpoch, OwnerRunId)`
  claim. An episode runs from `task` spawn (or revival) until `report`, which
  commits a structured state `handoff`, releases its `SubagentGovernor` budget
  lease, archives the actor, and delivers report mail to its parent. Downward DM
  mail to an archived child revives it into episode `n + 1` with its prior
  handoff rather than replaying a bloated raw transcript.
- **Strictly vertical channel plane ([ADR-0016](../adr/0016-channel-communication-plane.md)):**
  Team communication uses two event-minted channel kinds on the team-root log:
  unit broadcast `#announce-{8}` (leader-post only, no sibling visibility) and
  persistent pair `#DM-{8}` (parent ⇄ direct child). Tree depth is bounded by
  `MAX_SUBAGENT_DEPTH = 2`.
- **Independent stop governance ([ADR-0013](../adr/0013-user-assembled-agent-workflows.md), [ADR-0014](../adr/0014-workflowbundle-and-event-sourced-control.md), [ADR-0017](../adr/0017-workflow-on-unified-substrate.md)):**
  Workers never decide on their own that a multi-step objective is finished.
  `hya -p` (Goal mode), `hya loop` (Loop mode with `--while`/`--until` shell
  predicates and verifier/planner hooks), and compiled `*.hya.md` **Workflow**
  DAGs (`hya-workflow` → `WorkflowControl` → `SessionEngine`) enforce
  independent evaluator/verifier checks, retry/fallback routing, and hard
  iteration/budget ceilings in the engine.

### 6. Origin-Derived Tool Planes & Unified Bundle Model

- **Host-controlled tool planes ([ADR-0012](../adr/0012-one-agent-per-bundle-and-host-controlled-tool-plane.md)):**
  An Agent's visible Harness surface is derived from its **Origin**, never
  self-declared in a manifest. Trusted first-party agents (`hya/core-agents`)
  run on the `Full` plane (all 28 canonical builtin tools, Harness skills, MCP,
  and plugins, narrowed by `resource_view`). Installed public bundle agents run
  on the clamped `InternalPublic` plane plus only their own bundle's declared
  resources.
- **Unified `kind: Bundle` (prepared format v4) & process isolation ([ADR-0009](../adr/0009-rust-plugin-binaries-over-in-process-libraries.md)):**
  Twelve trusted first-party bundles ship with the backend (five lockstep Rust
  dylib tool-family presets, `core-agents`, `core-skills`, `core-commands`,
  `agent-channels`, `goal-loop`, `plan-impl-review`, `basic-tui-components`).
  Third-party extensions package as deterministic `.hyabundle` archives
  containing `agents[]`, optional `workflow`, `channels[]`, MCP declarations,
  out-of-process NDJSON JSON-RPC 2.0 sidecars (`kind: rust`, `kind: bun`,
  `kind: claude`), custom `/v1/bundles/.../api/*` routes, custom permission
  modes, and isolated TUI extensions (`@hya/tui-sdk`).

### 7. Hot Extension Reload vs Clean Stop/Start Restart

Per [ADR-0031](../adr/0031-hot-reload-and-stop-start-restart.md) (and
[ADR-0007](../adr/0007-hot-skill-reload-visibility.md) /
[ADR-0008](../adr/0008-hot-plugin-reload-visibility.md)):

- **Extensions hot-reload in place:** Installing/updating bundles, editing
  project plugins, or reconnecting MCP servers publishes a new immutable
  `ConfigGeneration` or `ScopeOverlay` without restarting the daemon; in-flight
  rounds keep their captured `TurnBinding`, and subsequent turns see the new
  catalog atomically.
- **Binary upgrades use `stop` → `start` + `pending_resume`:** Upgrading `hya`
  cleanly drains active turns (`FinishCause::Shutdown`), records interrupted
  root sessions in `pending_resume`, starts the new binary, and injects a
  structured restore prompt listing unfinished tool calls and interrupted
  subagents so the model can resume safely.

---

## Layer Boundaries

| Layer | Crates / Packages | Responsibility |
| --- | --- | --- |
| Foundation & Wire | [`hya-proto`](../../crates/hya-proto), [`hya-version`](../../crates/hya-version), [`hya-api`](../../crates/hya-api) | Strongly typed IDs, canonical `Event`/`Envelope`, `Message`/`Part`, deterministic `Projection` reducer, aggregate version constants, and generated `hya.v1` prost/tonic/pbjson types + error-code table. |
| Model I/O | [`hya-provider`](../../crates/hya-provider) | Normalize OpenAI Chat, OpenAI Responses, OpenAI Codex, Grok Build, Anthropic Messages, Google Gemini, scripted `FakeProvider`, and offline `DevProvider` (`hya/offline`) into canonical streamed `Event`s, with capability preflight and retry budgets. |
| Tools, MCP & Plugins | [`hya-tool`](../../crates/hya-tool), `bundles/presets/*-tools/native`, [`hya-mcp`](../../crates/hya-mcp), [`hya-plugin`](../../crates/hya-plugin), [`hya-plugin-bun`](../../crates/hya-plugin-bun), [`hya-plugin-claude`](../../crates/hya-plugin-claude) | 28 canonical builtin tools across 5 lockstep native tool families, `PermissionPlane` + `ProjectScope` multi-root guard, session service planes, MCP `mcp__server__tool` bridge, and out-of-process stdio JSON-RPC plugin hosts (Rust, Bun JS, Claude Code import). |
| Persistence | [`hya-store`](../../crates/hya-store) | SQLite WAL `event_log`, transactional batch appends, `projection_snapshot` cache, `project`/`project_root`, `resident_actor_claim` epoch fencing, `pending_resume`, `file_blob` revert snapshots, and separate `bundle_registry` DB. |
| Packages & Workflows | [`hya-bundle`](../../crates/hya-bundle), [`hya-workflow`](../../crates/hya-workflow) | Deterministic `.hyabundle` preparation/validation (format v4), 12 first-party bundle loaders, `BundleCatalog` indexing, and `*.hya.md` Workflow compilation into immutable Stage DAG plans. |
| Agent Runtime | [`hya-core`](../../crates/hya-core) | `SessionEngine`, turn admission & streaming rounds, 5-stage context compaction, file snapshots/revert/fork, `ResidentSupervisor` episodic actors, mailbox/channel service, `SubagentGovernor`, Goal/Loop drivers, durable Workflow execution, and `RuntimeRegistry` / `CatalogScope` overlays. |
| Composition & Control | [`hya-app`](../../crates/hya-app) | `config.yaml` + auth/OAuth resolution, model cache merge ([ADR-0029](../adr/0029-config-model-entries-override-the-model-cache.md)), MCP/plugin/LSP/formatter wiring, per-Project bundle/plugin watchers, `WorkflowControl`, and `HyaRuntime` / `BuiltSessionEngine` lifecycle. |
| Transports & CLI | [`hya-server`](../../crates/hya-server), [`hya-sdk-v1`](../../crates/hya-sdk-v1), [`hya-relay`](../../crates/hya-relay), [`hya-updater`](../../crates/hya-updater), [`hya-backend`](../../crates/hya-backend) | `/v1` HTTP/SSE/WS + `V1Grpc` single-port server, Host allowlist guard, ephemeral session reaper, typed Rust v1 SDK, `hya.relay.v1` + Noise `NKpsk0` proxy/bridge/host-connector, release installer (`hya update`), and the unified `hya` CLI binary. |
| Interactive Frontends | [`packages/hya-tui`](../../packages/hya-tui), [`packages/hya-tui-web`](../../packages/hya-tui-web), [`packages/hya-tui-sdk`](../../packages/hya-tui-sdk) | Bun/OpenTUI terminal UI, real-PTY + xterm.js browser WebUI host & `tui-exp` / Playwright test harness, and `@hya/tui-sdk` isolated TUI extension SDK. |
| Verification & Tooling | [`hya-e2e`](../../crates/hya-e2e), [`xtask`](../../crates/xtask) | Track P process-level E2E suite (real `hya` + FakeLlm) and workspace codegen/packaging/benchmark/release-rehearsal tooling. |

---

## End-to-End Turn Flow

1. **Session & Scope Resolution:** A client creates or resumes a `Session` bound
   to a `Project` (or `Temporary` scratch root), an `Agent`, and a `ModelRef`.
2. **Turn Admission:** `CreateTurn` (or headless `hya exec` / `-p` / `loop`)
   resolves the session's `CatalogScope` into an immutable `TurnBinding` (tools,
   skills, commands, guidance, model policy) and commits the user message events
   in one SQLite transaction.
3. **Context Preparation & Compaction:** `SessionEngine::run_turn` folds the
   session's `Projection`, applies configured context-reduction stages if token
   occupancy crosses threshold (see [Compaction](../compaction.md)), and builds
   a normalized `CompletionRequest`.
4. **Model Streaming:** `ProviderRouter` selects the matching provider route,
   runs capability preflight, and streams upstream SSE into canonical `Event`s.
   Text deltas publish live at `seq = 0`; completed text/reasoning parts and
   `ToolCallRequested` events commit durably (`seq > 0`).
5. **Permission-Gated Tool Execution:** Requested tool calls execute through
   `ToolRegistry` with a session-scoped `ToolCtx`. `PermissionPlane` evaluates
   mode (`manual`, `yolo`, or bundle mode), static rules, saved Project rules,
   and `ProjectScope` root containment — parking `Ask` decisions on the v1
   `Interactions` plane until approved or rejected.
6. **Multi-Round Loop & Completion:** Tool outputs pass through shape-aware caps
   and post-tool hooks, commit as `ToolResult`/`ToolError` events, and trigger
   the next provider round until a round finishes with no further tool calls, is
   cancelled, or hits an error — always closing with `MessageFinished` so
   clients never hang.

---

## Architecture Deep-Dive Map

| Document | Focus |
| --- | --- |
| [Architecture & Design Diagrams](diagrams.md) | Visual Mermaid & SVG diagrams covering topology, crate DAG, turn sequence, episodic subagents, Workflows, Projects, Relay, and hot-reload/restart. |
| [Project Structure](../project-structure.md) | Repository directory layout, crate/module tables, 28 builtin tools, and dependency direction. |
| [Domain Glossary (`CONTEXT.md`)](../../CONTEXT.md) | Ubiquitous vocabulary across `hya-proto`, `hya-core`, tools, Projects, Relay, and TUI. |
| [Runtime](runtime.md) | `SessionEngine`, publish seams, `TurnBinding`, Goal/Loop drivers, file snapshots & revert, and workspace roots. |
| [Event Model](event-model.md) | Strong IDs, `EventSeq` rules, full `Event` wire catalog, and `Projection` reducer invariants. |
| [Storage](storage.md) | SQLite WAL schema (`0001_init.sql`), batch append transactions, `projection_snapshot`, `pending_resume`, and `bundle_registry`. |
| [Subagent Orchestration](subagent-orchestration.md) | Episodic resident actors, `report`/`handoff`/`archive`/revive lifecycle, `#announce` & `#DM` channels, and Workflow execution on the unified substrate. |
| [Admission and Governor](admission-and-governor.md) | Active-claim capacity leases, atomic batch registration, and `SubagentGovernor` execution budgets. |
| [Providers](providers.md) | `Provider`/`Protocol`/`Decoder` traits, OpenAI/Responses/Codex/Grok/Anthropic/Google encoders, model cache overrides, and reasoning variants. |
| [Tools and Permissions](tools-and-permissions.md) | `ToolRegistry`, `PermissionPlane`, `ProjectScope` multi-root containment, saved rules, and output caps. |
| [Agent Tool Surface](agent-tool-surface.md) | Registered vs advertised vs executable tool contracts, `todo__*`, `task`, `send`/`wait`/`report`/`archive`, and hashline editing. |
| [Server and Client](server-client.md) | Consolidated `hya.v1` HTTP/SSE/WS + gRPC single-port server, `AppState`, error codes, and `hya-sdk-v1`. |
| [Protocol Guide](../protocol/README.md) & [API Reference](../protocol/api-reference.md) | Wire-level `hya.v1` integration guide and generated 18-service / 107-RPC specification. |
| [Architecture Decision Records (`docs/adr/`)](../adr/) | Accepted ADRs governing daemon lifecycle, Projects, Relay, episodic subagents, TUI/WebUI, and hot reload. |
