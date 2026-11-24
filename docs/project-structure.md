# Project Structure

`hya` is organized as a Rust workspace of focused crates paired with self-contained
Bun/TypeScript packages for the terminal UI, browser WebUI host, TUI extension
SDK, and out-of-process JS/Claude bundle adapters. Every runtime surface shares
one canonical event model (`hya-proto::Event` / `Projection`) and one public
wire contract (`hya.v1`).

For visual dependency and runtime flow diagrams, see
[Architecture & Design Diagrams](architecture/diagrams.md) and
[Design Philosophy & Architecture Overview](architecture/overview.md).

## Repository Map

| Path | Purpose |
| --- | --- |
| [`../Cargo.toml`](../Cargo.toml), [`../versions.toml`](../versions.toml) | Workspace members, shared dependencies, Rust `1.91` / edition `2024`, clippy lints (`unwrap_used`/`expect_used` denied), and the unified `hya` release version + minimum compatible backend version. |
| [`../CONTEXT.md`](../CONTEXT.md) | Ubiquitous domain glossary across runtime, store, tools, Projects, Relay, Workflows, and TUI. |
| [`../proto/hya/v1`](../proto/hya/v1), [`../proto/hya/relay/v1`](../proto/hya/relay/v1) | Canonical protobuf IDLs for the `hya.v1` client/server contract (18 services, 107 RPCs) and the `hya.relay.v1` rendezvous protocol. |
| [`../crates`](../crates) | 21 Rust crates: runtime, providers, tools, MCP, plugins, store, server, SDK, relay, updater, unified `hya` binary, E2E harness, and `xtask`. |
| [`../bundles/presets`](../bundles/presets), [`../bundles/first-party`](../bundles/first-party), [`../bundles/internal`](../bundles/internal) | The 13 trusted first-party bundles (including the 4 native Rust tool-family library crates under `bundles/presets/*-tools/native` and the internal `hya/compaction` library crate under `bundles/internal/compaction/native`). |
| [`../bundles/extra`](../bundles/extra), [`../bundles/examples`](../bundles/examples) | Optional installable `hya-extra/*` bundles (`zvec-grep`, `scout`, `jev-model-router`, `model-fallback`, `token-summary`) and WorkflowBundle examples (`argus-example`). |
| [`../packages/hya-tui`](../packages/hya-tui) | Interactive Bun/OpenTUI terminal frontend over `hya.v1` HTTP/JSON+SSE or direct gRPC (shipped as `lib/hya/tui`). |
| [`../packages/hya-tui-web`](../packages/hya-tui-web) | Generic real-PTY + xterm.js browser WebUI host and `tui-exp` / Playwright terminal test harness (shipped as `lib/hya/tui-web`). |
| [`../packages/hya-tui-sdk`](../packages/hya-tui-sdk) | `@hya/tui-sdk` package for authoring isolated bundle TUI extensions (`defineTuiExtension`). |
| [`../plugins`](../plugins) | Standalone full-stack plugin examples such as [`disk-inspector`](disk-inspector.md). |
| [`../scripts/hya-install.sh`](../scripts/hya-install.sh), [`../install.sh`](../install.sh) | Hosted release installer (also embedded in `hya update`) and local source-checkout installer. |
| [`../docs`](../docs) | Architecture, user/configuration guides, diagrams, generated `hya.v1` protocol specs, ADRs, and developer guidelines. |

## Rust Crates (`crates/` & Native Bundles)

| Crate | Source | Responsibility |
| --- | --- | --- |
| `hya-version` | [`../crates/hya-version`](../crates/hya-version) | Compile-time unified `VERSION` (`BACKEND_VERSION` / `FRONTEND_VERSION`) and `MINIMUM_BACKEND_VERSION` constants parsed from `versions.toml`. |
| `hya-proto` | [`../crates/hya-proto`](../crates/hya-proto) | Dependency-light domain & wire types: prefixed strong IDs (`hysec_`, `msg_`, `part_`, `tc_`, `prj_`, …), `Event`/`Envelope`, `Message`/`Part`, mail/scope types, and the deterministic `Projection` reducer. |
| `hya-api` | [`../crates/hya-api`](../crates/hya-api) | Generated `hya.v1` Rust types (prost/tonic/pbjson), HTTP status ↔ gRPC code error table, and pagination cursor helpers (`cargo run -p xtask -- gen-api`). |
| `hya-provider` | [`../crates/hya-provider`](../crates/hya-provider) | `Provider`/`Protocol`/`Decoder` traits, `ProviderRouter`, HTTP/SSE client, OpenAI Chat/Responses/Codex, Grok Build, Anthropic, Google Gemini, scripted `FakeProvider`, and offline `DevProvider`. |
| `hya-tool` | [`../crates/hya-tool`](../crates/hya-tool) | `Tool` trait, 25-name canonical `ToolRegistry` + hidden aliases, native dylib loader, the `Rewriter` contract and host services for native bundles, token estimation, `PermissionPlane`, `ProjectScope` multi-root containment, output caps, and session planes (`Interaction`, `Spawner`, `Skill`, `WebSearch`, `Lsp`, `Formatter`, `Workflow`, `Mailbox`). |
| `hya-*-tools` (4 crates) | [`../bundles/presets`](../bundles/presets) | Concrete Rust implementations of the 25 registry tools across `hya-base-tools`, `hya-extended-tools`, `hya-channel-tools`, and `hya-network-tools`. |
| `hya-compaction` | [`../bundles/internal/compaction/native`](../bundles/internal/compaction/native) | The `hya/compaction` internal bundle: the `shake` and `remote` context-reduction steps the kernel runs before its own Rewrite, as a native `Rewriter`. |
| `hya-child-io` | [`../crates/hya-child-io`](../crates/hya-child-io) | Shared bounded NDJSON framing and connection task tracking for MCP and plugin processes. |
| `hya-mcp` | [`../crates/hya-mcp`](../crates/hya-mcp) | MCP stdio/HTTP client & manager, desired/observed/effective reconciliation, and `mcp__<server>__<tool>` bridge into `hya-tool`. |
| `hya-plugin` | [`../crates/hya-plugin`](../crates/hya-plugin) | Out-of-process NDJSON JSON-RPC 2.0 plugin host (`PluginHost`, `PluginConn`), manifest/config loading, typed hook and bundle API calls, and tool/permission bridges. |
| `hya-plugin-bun` | [`../crates/hya-plugin-bun`](../crates/hya-plugin-bun) | `adapter/` Bun runtime loading JS bundle extensions (`--bundle-extension` / `--extension`). |
| `hya-plugin-claude` | [`../crates/hya-plugin-claude`](../crates/hya-plugin-claude) | Claude Code plugin directory / marketplace importer and runtime bridge (`hya bundle install --claude`). |
| `hya-store` | [`../crates/hya-store`](../crates/hya-store) | SQLite WAL session store (`event_log`, `session`, `project`/`project_root`, `saved_permission`, `resident_actor_claim`, `pending_resume`, `projection_snapshot`, `file_blob`) plus separate `bundle_registry` DB. |
| `hya-bundle` | [`../crates/hya-bundle`](../crates/hya-bundle) | Unified `kind: Bundle` (prepared format v4) source parser, validator, deterministic `.hyabundle` 7z packager, 13 first-party bundle loaders, and `BundleCatalog`. |
| `hya-workflow` | [`../crates/hya-workflow`](../crates/hya-workflow) | `*.hya.md` Workflow parser, graph validator (`flowchart TD`), normalization, topological levels, model assignments, and canonical revisions. |
| `hya-core` | [`../crates/hya-core`](../crates/hya-core) | The three-primitive kernel (`kernel`, run alone by `hya exec --bare`), the internal native bundle loader (`bundles`), and the shared `CoreError`. |
| `hya-engine` | [`../crates/hya-engine`](../crates/hya-engine) | Full-mode session engine on the kernel: `SessionEngine`, turn admission/streaming rounds, compaction settings and the terminal-handoff summarizer (the context-reduction steps are the `hya/compaction` bundle), file snapshots/revert/fork, `ResidentSupervisor` episodic actors, vertical channel service, `SubagentGovernor`, Goal/Loop drivers, durable Workflow execution, and `RuntimeRegistry` / `CatalogScope` overlays. |
| `hya-app` | [`../crates/hya-app`](../crates/hya-app) | Runtime composition: `config.yaml` + auth/OAuth, model cache merge, MCP/plugin/LSP/formatter wiring, engine adapters for plugin hosts, per-Project `.hya/bundles` & `.hya/plugins` watchers, `WorkflowControl`, and `HyaRuntime` / `BuiltSessionEngine`. |
| `hya-server` | [`../crates/hya-server`](../crates/hya-server) | Single-port `/v1` HTTP/JSON+SSE+WS router + `V1Grpc` tonic dispatcher, Host allowlist guard, Projects & session placement, ephemeral session reaper, and `relay_host` connector. |
| `hya-sdk-v1` | [`../crates/hya-sdk-v1`](../crates/hya-sdk-v1) | Typed Rust client SDK over `hya.v1`: bootstrap, sessions, event-driven turns, live SSE `StreamFrame` subscription, `V1SessionMirror`, and `Client` harness helpers. |
| `hya-relay` | [`../crates/hya-relay`](../crates/hya-relay) | Runtime-independent secure relay (`hya.relay.v1`), `RelayLink` (`hya://…`), Ed25519 Room ownership, `Noise_NKpsk0` encrypted streams, `hya proxy` core, and gRPC/WebSocket bindings (`t=auto`). |
| `hya-updater` | [`../crates/hya-updater`](../crates/hya-updater) | Standalone `hya update` command surface embedding `scripts/hya-install.sh` with zero runtime-crate dependencies. |
| `hya-backend` (`bin/hya`) | [`../crates/hya-backend`](../crates/hya-backend) | Unified `hya` CLI executable: bare `hya` daemon discovery + TUI/WebUI process orchestration, `exec`, `-p`, `loop`, `serve`, `sessions`, `tail-session`, `provider`/`login`/`oauth`/`models`, `agent`/`bundle`/`workflow`, `proxy`/`bridge`/`relay`, and `update`. |
| `hya-e2e` | [`../crates/hya-e2e`](../crates/hya-e2e) | Track P process-level E2E harness (real `hya` + scripted FakeLlm; matrix in `crates/hya-e2e/matrix.toml`). |
| `xtask` | [`../crates/xtask`](../crates/xtask) | Dev tooling (`cargo run -p xtask -- <task>`): `gen-api`, `gen-relay`, `matrix-check`, `package-bundle`, `package-native-tool-bundle`, `package-native-tool-library`, `stage-first-party-bundles`, `startup-bench`, `lifecycle-bench`, and `release-rehearsal`. |

## Canonical Builtin Tool Surface (25 Registry Tools and 2 Kernel Tools)

`ToolRegistry::builtins()` loads the four trusted first-party tool-family bundles
into **25 canonical schema names** (`wait` is declared by both `extended-tools`
and `channel-tools`, with `channel-tools` overriding to add mail wake):

| Family Bundle | Canonical Tools (Hidden Runtime Aliases) | Permission Action | Summary |
| --- | --- | --- | --- |
| `hya/base-tools` | `read` | `Read` | Hashline/raw file reads, media attachments, directory listings (`ProjectScope` guarded). |
| `hya/base-tools` | `write`, `edit`, `apply_patch` (`patch`) | `Edit` | Atomic writes, anchored hashline edits with stale recovery, unified patches, post-edit formatter/LSP hooks. |
| `hya/base-tools` | `ls` | `Read` | Immediate directory listing. |
| `hya/base-tools` | `glob`, `find` | `Glob` | Bounded glob and filename search. |
| `hya/base-tools` | `grep` | `Grep` | Native cancellable regex/literal content search with hashline context rows. |
| `hya/base-tools` | `bash` (`shell`) | `Bash` | Bounded command execution (pipe or real PTY) with process-group cancellation and spill artifacts. |
| `hya/base-tools` | `ask_user` (`question`) | `Tool` | Batched structured select/free-text questions over `InteractionPlane`. |
| `hya/extended-tools` | `lsp` | `Lsp` | Workspace symbols, definitions, references, diagnostics via `LspPlane`. |
| `hya/extended-tools` | `skill` | `Skill` | Progressive-disclosure `SKILL.md` loader. |
| `hya/extended-tools` | `list_agents`, `search_agent`, `project_activity` | `ReadOnly` | Spawnable agent discovery, archived direct-child handoff search, and Project-wide session/file activity. |
| `hya/extended-tools` | `task`, `archive` | `Task` | Non-blocking batch resident subagent spawn and live descendant cancellation/archival. |
| `hya/extended-tools` | `workflow`, `plan_exit` (`plan`) | `Tool` | Governed `WorkflowControl` operations (`list`/`info`/`select`/`run`/`state`) and plan-mode exit signal. |
| `hya/channel-tools` | `send`, `report` | `Tool` | Vertical channel mail (`#announce` broadcast or `#DM` / handle / `^parent`, reviving archived children) and terminal episode report. |
| `hya/channel-tools` | `list_channel`, `wait` | `ReadOnly` | Caller channel listing and bounded blocking wait on child reports or inbound mail (overrides `extended-tools` `wait`). |
| `hya/network-tools` | `webfetch` (`fetch`), `websearch` (`search`) | `WebFetch` / `WebSearch` | URL fetch and configured Exa/Parallel web search. |
| Kernel | `todo`, `rewrite` | Kernel | Update session completion criteria and rewrite the live transcript. |
| Builtin fallback | `invalid` | `Tool` | Structured error response when a model emits an unknown tool name or malformed JSON arguments. |

## Trusted First-Party Bundles (13 Total)

Loaded from `bundles/presets/*`, `bundles/first-party/*`, and `bundles/internal/*` in Cargo builds, or
`<prefix>/bundles/hya-<name>.hyabundle` in installed layouts:

1. `hya/base-tools` — core file, search, shell, and question tools
2. `hya/extended-tools` — LSP, skill, subagent spawn/archive, workflow, and Project activity tools
3. `hya/network-tools` — `webfetch` and `websearch`
4. `hya/channel-tools` — `send`, `list_channel`, `report`, and mail-aware `wait`
5. `hya/core-agents` — built-in agents (`hya-main`, `hya-plan`, `hya-task`, `hya-scout`, `hya-reviewer`, plus reserved `hya-title`)
6. `hya/core-skills` — built-in authoring and self-update skills
7. `hya/core-commands` — built-in slash prompt templates
8. `hya/agent-channels` — default declarative vertical channel policy
9. `hya/goal-loop` — first-party Goal/Loop Workflow bundle
10. `hya/plan-impl-review` — first-party Plan → Implement → Review Workflow bundle
11. `hya/basic-tui-components` — first-party TUI extension components
12. `hya/code-mode` — grouped domain tool execution
13. `hya/compaction` — internal context reduction

## Dependency Direction

```text
                       hya-proto   hya-version   hya-api (proto/hya/v1)
                         ^   ^   ^       ^              ^
            +------------+   |   +-------+------+       |
            |                |           |      |       |
      hya-provider       hya-tool    hya-store  |   hya-relay (proto/hya/relay/v1)
                         ^   ^  ^        ^      |       ^
       4x native tools --+   |  |        |      |       |
                 hya-mcp ----+  |        |      |       |
              hya-plugin -------+        |      |       |
     (hya-plugin-bun / claude)           |      |       |
                                    hya-bundle  |       |
                                    hya-workflow|       |
                                         ^      |       |
                                         |      |       |
                                      hya-core -+       |
                                         ^              |
                                         |              |
                                      hya-app     hya-server <--> hya-sdk-v1
                                         ^         ^    ^
                                         |         |    |
                              hya-backend (bin/hya) +----+ (+ hya-updater)
                                         |
                      spawns child processes (bare `hya`):
                   packages/hya-tui  +  packages/hya-tui-web
```

Lower crates never depend on `hya-backend` or frontend packages; `hya-relay` and
`hya-updater` have zero dependencies on runtime crates (`hya-core`, `hya-app`,
`hya-store`).
