# hya Documentation

`hya` is an event-sourced Rust multi-agent coding runtime. Every client surface
speaks one consolidated contract — **`hya.v1`** (18 services / 107 RPCs) — over
HTTP/JSON + SSE + WebSocket (`/v1`) and gRPC on a single port, backed by a
persistent per-database daemon and optional end-to-end encrypted `Noise_NKpsk0`
relay access.

The documentation is organized into **three core categories**, followed by
protocol, bundle/extension, and contributor references.

---

## 1. Design Philosophy & Architecture (`的设计理念与架构`)

Start here to understand *why* `hya` is structured around a single canonical
event log, how crates and runtime planes fit together, and what invariants
govern sessions, Projects, subagents, and Workflows:

- **[Design Philosophy & Architecture Overview](architecture/overview.md)** — The seven core architectural pillars, layer boundaries, and end-to-end turn lifecycle.
- **[Project Structure](project-structure.md)** — Repository layout, all 21 workspace crates + 5 native tool-family crates, 28 canonical builtin tools, 12 first-party bundles, and dependency direction.
- **[Domain Glossary (`CONTEXT.md`)](../CONTEXT.md)** — Canonical domain terminology across `hya-proto`, `hya-core`, Projects, Relay, Workflows, and TUI.
- **Core Subsystem Specifications:**
  - [Runtime (`hya-core`)](architecture/runtime.md) — `SessionEngine`, publish seams, `TurnBinding`, Goal/Loop drivers, file snapshots & revert, and workspace roots.
  - [Event Model (`hya-proto`)](architecture/event-model.md) — Strong IDs, `EventSeq` (`seq = 0` live vs `seq > 0` durable), full `Event` wire catalog, and deterministic `Projection` reducer.
  - [Storage (`hya-store`)](architecture/storage.md) — SQLite WAL schema (`0001_init.sql`), atomic batch appends, `projection_snapshot`, `pending_resume`, and `bundle_registry`.
  - [Subagent Orchestration](architecture/subagent-orchestration.md) — Episodic resident actors, `report` / `handoff` / `archive` / downward-DM revival, and vertical `#announce` / `#DM` channels.
  - [Admission and Governor](architecture/admission-and-governor.md) — Durable `admission_journal` idempotency and in-memory `SubagentGovernor` lease budgets.
  - [Providers (`hya-provider`)](architecture/providers.md) — OpenAI Chat/Responses/Codex, Grok Build, Anthropic, Google Gemini, capability preflight, and reasoning routing.
  - [Tools and Permissions (`hya-tool`)](architecture/tools-and-permissions.md) — `ToolRegistry`, `PermissionPlane`, `ProjectScope` symlink-safe multi-root containment, saved rules, and output caps.
  - [Agent Tool Surface](architecture/agent-tool-surface.md) — Registered vs advertised vs executable schemas, `todo__*`, `task`, channel tools, and hashline editing.
  - [Server and Client (`hya-server` / `hya-sdk-v1`)](architecture/server-client.md) — Single-port `/v1` HTTP/SSE/WS + `V1Grpc` dispatch, `AppState` drain, error codes, and SDK clients.
- **Architecture Decision Records (`docs/adr/`):**
  - Daemon, Projects & Lifecycle: [ADR-0022 (One writer per DB)](adr/0022-one-writer-per-database.md), [ADR-0023 (Persistent daemon)](adr/0023-persistent-backend-daemon.md), [ADR-0024 (Projects & client workspace)](adr/0024-project-model-and-client-chosen-workspace.md), [ADR-0026 (Multi-root path permissions)](adr/0026-multi-root-path-permissions.md), [ADR-0027 (Per-Project catalog tier)](adr/0027-per-project-catalog-tier.md), [ADR-0031 (Hot reload & stop/start restart)](adr/0031-hot-reload-and-stop-start-restart.md).
  - Multi-Agent, Mailbox & Workflows: [ADR-0001 (Event-sourced mailbox)](adr/0001-event-sourced-mailbox-and-channels.md), [ADR-0002 (Resident actor fencing & budgets)](adr/0002-resident-actor-model-and-autonomous-main-agent.md), [ADR-0013 (Compiled Workflows)](adr/0013-user-assembled-agent-workflows.md), [ADR-0014 (WorkflowBundle & control)](adr/0014-workflowbundle-and-event-sourced-control.md), [ADR-0015 (Unified episodic subagent lifecycle)](adr/0015-unified-resident-subagent-lifecycle.md), [ADR-0016 (Vertical channel plane)](adr/0016-channel-communication-plane.md), [ADR-0017 (Workflow on unified substrate)](adr/0017-workflow-on-unified-substrate.md).
  - Models, Bundles, Plugins & Relay: [ADR-0004 (Model categories)](adr/0004-model-category-resolution-and-precedence.md), [ADR-0007 (Hot skill reload)](adr/0007-hot-skill-reload-visibility.md), [ADR-0008 (Plugin refresh visibility)](adr/0008-hot-plugin-reload-visibility.md), [ADR-0009 (Out-of-process plugin binaries)](adr/0009-rust-plugin-binaries-over-in-process-libraries.md), [ADR-0012 (Origin-controlled tool plane)](adr/0012-one-agent-per-bundle-and-host-controlled-tool-plane.md), [ADR-0025 (Secure relay)](adr/0025-secure-relay.md), [ADR-0029 (Config model cache overrides)](adr/0029-config-model-entries-override-the-model-cache.md).
  - TUI & WebUI: [ADR-0018 (Browser PTY environment)](adr/0018-browser-rendered-tui-test-environment.md), [ADR-0019 (OpenTUI frontend)](adr/0019-adopt-opentui-frontend.md), [ADR-0020 (Bundled TUI & WebUI in bare `hya`)](adr/0020-bundle-tui-and-webui-in-hya.md), [ADR-0021 (WebUI desktop notifications)](adr/0021-webui-desktop-notifications.md), [ADR-0030 (WebUI tab titles)](adr/0030-webui-tab-title-follows-terminal-title.md).

---

## 2. Usage Guides: Quick Start & Configuration (`的使用方法与配置指南`)

Everything needed to install, configure, operate, and extend `hya` day to day:

- **[Quick Start Guide](getting-started.md)** — Release & source installation, launching the TUI + WebUI, connecting live providers / OAuth, headless `exec` / `-p` / `loop` runs, session replay, and remote Relay setup.
- **[Configuration Guide](configuration.md)** — Complete `~/.config/hya/config.yaml` map, offline fallback, providers/auth/OAuth, remembered Agent models & reasoning effort, model categories, permissions, MCP, plugins, websearch, subagent budgets, formatter/LSP, and `HYA_*` environment variables.
- **Installation & Operations:**
  - [Install and Update](install.md) — Split backend/frontend release archives, `https://hya.ed-aisys.com/install.sh`, `hya update`, version pinning, and rollback.
  - [Install on Windows with WSL](install-wsl.md) — WSL 2 setup and accessing the WebUI from Windows browsers.
  - [CLI Reference](cli.md) — Every `hya` subcommand, flag, database lock rule, and exit code.
  - [Troubleshooting](troubleshooting.md) — Solutions for daemon locks, WebUI ports, Host allowlist (`403`), Relay diagnostics, and provider errors.
- **Interactive Surfaces & Remote Access:**
  - [OpenTUI Frontend](tui.md) — Layout, Projects sidebar, `/key` Provider View, `/agent`, `/subagents`, `/workflow`, `/rules`, `/diff`, Vim mode, and keybindings.
  - [Browser-Rendered TUI (WebUI)](tui-web.md) — Running the TUI in browsers over a real PTY + xterm.js, plus the `tui-exp` and Playwright test harnesses.
  - [Secure Relay](relay.md) — End-to-end encrypted remote access with `hya proxy`, `hya serve --relay`, `hya --connect`, `hya bridge`, and `hya relay doctor`.
- **Context, Skills, Workflows & Bundles:**
  - [Compaction](compaction.md) — The five context-reduction mechanisms, thresholds, and configurable order.
  - [Skills](skills.md) — Skill discovery roots, progressive loading via `skill`, and `hya/core-skills`.
  - [Workflows](workflows.md) — Authoring `*.hya.md` Stage DAGs, stage model routing, CLI/TUI control, and WorkflowBundle packaging.
  - [Goal and Loop Authoring](goal-loop-authoring.md) — Independent evaluators, shell `--while`/`--until` predicates, and plugin hooks.
  - [Bundle Authoring](agent-bundle-authoring.md) & [Bundle Runtime](bundle-runtime.md) — Authoring, packaging, installing, and running `.hyabundle` (format v4) static and Bun/Rust sidecar bundles (examples under [`examples/`](examples/)).
  - [Core Agents](core-agents.md), [Tool-Family Presets](base-tools.md), [Subagent Bundles](subagent-bundles.md), [Agent Channels](agent-channels.md), [Extra Bundles](extra-bundles.md), and [Claude Plugin Import](claude-plugin-import.md).
  - [Plugin Protocol](plugin-protocol.md), [Bundle-Owned TUI Extensions](tui-extensions.md), [TUI Extension SDK (`@hya/tui-sdk`)](tui-extension-sdk.md), and [Disk Inspector Plugin](disk-inspector.md).

---

## 3. Architecture & Design Diagrams (`的设计图`)

Visual blueprints and protocol/lifecycle diagrams for the current `hya` system:

- **[Architecture & Design Diagrams](architecture/diagrams.md)** — Includes:
  1. High-Level System Architecture Blueprint ([SVG](../docs/assets/hya-architecture.svg))
  2. Process Topology, Persistent Daemon & Client Surfaces
  3. Workspace Crate & Package Dependency Graph
  4. Single-Turn Event-Sourced Execution & Streaming Sequence (`seq = 0` vs `seq > 0`)
  5. Episodic Resident Subagent Lifecycle & Vertical Channel Plane (`#announce` & `#DM`)
  6. Governed Execution Modes (Turn vs Goal vs Loop vs Workflow DAG)
  7. Multi-Root `Project`, `ProjectScope` & Lazy `CatalogScope` Overlays
  8. Zero-Trust End-to-End Encrypted Relay (`hya.relay.v1` + `Noise_NKpsk0`)
  9. Extension Hot-Reload vs Binary Stop/Start & `pending_resume` Recovery

---

## Protocol & Contributor References

| Area | Pages |
| --- | --- |
| `hya.v1` Wire Contract | [Protocol Guide](protocol/README.md) · [Generated API Reference](protocol/api-reference.md) · [OpenAPI Schema](protocol/openapi.json) |
| Workspace Development & Release | [Development Guide](development.md) · [Backend Spec Guidelines](spec/backend/index.md) · [Cross-Layer & Reuse Guides](spec/guides/index.md) |
| Verification & E2E Matrix | [Testing Overview](testing/README.md) · [Agent Feature Matrix](testing/agent-matrix.md) · [Process E2E Harness](testing/process-e2e.md) |
