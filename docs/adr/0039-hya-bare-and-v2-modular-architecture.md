# ADR-0039: `hya-bare`, modular `bundles/builtin/`, and clean-sheet `hya.v2` architecture

Date: 2026-11-27
Status: Accepted

## Context

[ADR-0033](0033-three-primitive-kernel.md) introduced the three-primitive kernel (`model append`, `tool append`, `rewrite`) inside `hya-core`. [ADR-0034](0034-context-is-m-g-and-w.md) anchored the kernel Context as $(m, g, w)$ read through a recorded `View` (`todo` as $g$, workspace file observations as $w$). [ADR-0035](0035-full-mode-follows-the-kernel.md) aligned full mode with the kernel so context reduction, prompt construction, and `todo`/`rewrite` reservation follow one path. [ADR-0038](0038-hyabundle-standard-and-independent-ci.md) consolidated built-in packages under `bundles/builtin/`, introduced `hyabundle` `schema_version: 2`, and separated bundle CI.

Despite those steps, Hya v1 remained structurally coupled across four boundaries:

1. **Incomplete bare mode:** Only `hya exec --bare` ran the kernel alone. `hya serve` and the interactive TUI/WebUI could not boot bare, `hya-bare` lacked a standard `grep` primitive and `AGENTS.md`/`skills`/`MCP` adapters, and `code-mode` lived in a separate built-in bundle rather than core.
2. **Leaky native ABI and fat core crates:** Stateful subsystems (`hashline`, `lsp`/`formatter`, `plan`, `goal`, `loop`, `workflow`, `agent-channels`, `multi-agent`) lived inside `hya-engine`, `hya-server`, and a 20-plane `ToolCtx`, or inside tool-only `bundles/builtin/*-tools` libraries. Native `cdylib` modules had no host-runtime abstraction for reading/writing session events, private KV/claims, model calls, or HTTP/gRPC endpoints without linking `hya-store`, `sqlx`, `hya-engine`, or `hya-server`.
3. **Domain-coupled v1 protocol, storage, and UI:** `proto/hya/v1` baked domain RPCs (`Workflow`, `Session.ArchiveSubagent`, `Session.SteerSubagent`) into the core contract; `hya_proto::Event` carried 25+ domain-specific variants; `hya-store` maintained domain tables such as `resident_actor_claim`; `hya-bundle` retained v1 compatibility (`extensions.*`, `exposure.yaml`, `BaseToolScheme`); `config.yaml` mixed core configuration with domain settings; and `packages/hya-tui` hardcoded `subagents`/`workflows` panes and `legacyPaneLayout.ts`.
4. **In-place migration drag:** Carrying v1 schema shims inside `hya-store`, `hya-bundle`, and `hya-server` prevented clean-sheet contracts and complicated testing across bare and trimmed configurations.

This ADR records the clean-sheet **v2** architecture: `hya` as a complete `hya-bare` core binary composed with modular `bundles/builtin/` via `hya-runtime-abi` v2, a clean `hya.v2` wire/storage/bundle/TUI stack, and an isolated `hya-migration` CLI for v1-to-v2 upgrades.

## Decision

### 1. Single Binary, Composable Startup (`hya-bare` + `bundles/builtin/` = `hya-full`)

- **`hya` is `hya-bare`.** Enabled bundles from `bundles/builtin/` compose `hya-full` by default at runtime.
- **Global `--bare`:** `--bare` is promoted to a global CLI flag covering `hya exec`, `hya serve`, and interactive TUI/WebUI (`packages/hya-tui` + `packages/hya-tui-web`). Under `--bare`, zero `bundles/builtin/` modules or TUI bundles are loaded.
- **Profile & override control in `config.yaml` v2:** `config.yaml` (`schema_version: 2`) controls built-in composition via:

```yaml
schema_version: 2
bundles:
  builtin:
    profile: full # or bare
    enable: []
    disable: []
```

  Parsed via a non-panicking `BuiltinBundleProfile`, `enable` and `disable` allow fine-grained trimming (for example disabling `hya/network` for air-gapped environments or disabling `hya/hashline`), and user-installed bundles or project `.hya/bundles/` may override built-in bundle definitions.

### 2. Minimal, Complete `hya-bare` Scope

`hya-bare` is a self-sufficient coding agent even with all built-in bundles disabled:

- **3D Context & three-primitive kernel:** `hya-core` preserves Context $(m, g, w)$ ([ADR-0034](0034-context-is-m-g-and-w.md)) and the three primitives (`model append`, `tool append`, `rewrite`) ([ADR-0033](0033-three-primitive-kernel.md)).
- **Kernel-exclusive primitives (`todo`, `rewrite`):** `todo` and `rewrite` live exclusively in `hya-core` and are permanently reserved; no bundle may override, shadow, or alias either tool name.
- **Standard base primitives:** `hya-core` provides standard implementations of `read`, `write`, `edit`, `bash`, and `grep` wired with `PermissionPlane`, `ProjectScope`, output capping, and a generic `HandleRouter` (`SchemeHandler`). Core base primitives contain zero hashline logic, zero `channel://` handling, and zero hardcoded LSP or Formatter logic.
- **Promoted `code-mode`:** Code Mode (`packages/hya-code-mode` + `crates/hya-app/src/code_mode.rs`) moves from `bundles/builtin/code-mode` into `hya-bare` core. It exposes `tools.*` and `workspace.state` by default and dynamically omits `agent()` when `hya/multi-agent` is not loaded.
- **Standard adapters & minimal bare TUI:** `hya-bare` retains `AGENTS.md` discovery and `InitialContextRecorded` injection, `skills` discovery and the built-in `skill` tool, `MCP` (`mcp__*` namespaced tools), `tools`/`bundles`/`plugins` runtime adapters, and a minimal `packages/hya-tui` shell that boots cleanly with zero TUI bundles.

### 3. Standard Primitives (`hya-bare`) vs `hya/hashline` (`overrides: hya/bare`)

Hashline-addressed file editing is decoupled from the core binary:

| Capability | `hya-bare` (core `hya-core`) | `hya/hashline` (`bundles/builtin/hashline`) |
| --- | --- | --- |
| `read`, `write`, `edit`, `grep` | Standard offset/line-based primitives with `PermissionPlane`, `ProjectScope`, output capping, `HandleRouter` | Content-hashed line-addressed primitives + `HashlineRuntime` (`overrides: hya/bare`) |
| `bash`, `todo`, `rewrite`, `skill` | Kernel / core implementations | Unchanged (`todo` and `rewrite` cannot be overridden) |

`hya/hashline` declares `overrides: hya/bare` in its `schema_version: 2` bundle manifest. When `hya/hashline` is disabled via `--bare` or `bundles.builtin.disable: [hya/hashline]`, `hya-bare`'s standard `read`/`write`/`edit`/`grep` run cleanly with zero core code paths or flags needed for hashline.

### 4. Full Native Extraction via `crates/hya-runtime-abi` (ABI v2)

Fulfilling the `abi_version: 2` placeholder reserved in [ADR-0038](0038-hyabundle-standard-and-independent-ci.md), a lightweight `crates/hya-runtime-abi` crate defines Lockstep Native ABI v2 and `hya-tool-loader` upgrades to v2 verification and loading.

- **Exported ABI v2 symbols** (generated via `export_native_bundle_v2!`):
  - `hya_native_abi_v2`
  - `hya_native_with_runtime_v2`
  - `hya_native_share_deadline_v2`
  - `hya_native_register_v2`
- **Host-injected runtime (`Arc<dyn HostRuntime>`):** Native bundles interact with the host solely through trait objects injected via `Arc<dyn HostRuntime>`:
  - `HostSessionStore` — read/append session events (`Event::Extension`) and read folded projections
  - `HostBundleStorage` — scoped KV state (`bundle_state`) and exclusive claims (`bundle_claim`)
  - `HostTurnRunner` — drive sub-turns or driver-controlled loops without importing `hya-engine`
  - `HostModelCaller` — invoke model routes (e.g., compaction remote calls, evaluators, verifiers, title/handoff)
  - `HostEventBus` — subscribe to and emit extension stream events
  - `HostCatalogReader` — inspect active agents, skills, commands, and tools
  - `HostExtensionRegistry` — query sibling extension registrations
- **Unified registration (`hya_native_register_v2`):** A single native bundle registration yields any combination of:
  - `tools` (with `HandleRouter` / `ToolCtx` slimmed to core planes)
  - `rewriters` (`Rewriter` extensions invoked by the kernel before its 80% `Rewrite`, [ADR-0035](0035-full-mode-follows-the-kernel.md))
  - `hooks` (`NativeHook`)
  - `services` (`NativeService`)
  - `apis` (`NativeBundleApiHandler`, mounted uniformly onto `BundleApi`)
  - `schemas` (`UriSchemaHandler` plugged into `HandleRouter`)
  - `reducers` (`ProjectionReducer` folding `Event::Extension` into `Projection::extensions`)
  - `goal_drivers` (`GoalDriver`)
  - `loop_drivers` (`LoopDriver`)
- **Slimmed core runtime:** `hya-engine` and `hya_tool::ToolCtx` are slimmed to a pure single-agent v2 core plus extension host adapters.
- **Target `bundles/builtin/` lineup (all `schema_version: 2`):**

| Legacy Bundle(s) | Target Bundle (`bundles/builtin/`) | Runtime | Capabilities & Role |
| --- | --- | --- | --- |
| `base-tools` | `hya/hashline` | `native` | Hashline `read`/`write`/`edit`/`grep` + `HashlineRuntime` (`overrides: hya/bare`) |
| `base-tools` + `extended-tools` | `hya/coding-extras` | `native` | `ls`/`glob`/`find`/`apply_patch`/`ask_user`/`lsp`/`project_activity` + LSP/Formatter `NativeService` & `tool.execute.after` hook |
| `compaction` | `hya/compaction` | `native` | `shake` + `remote` `Rewriter` on ABI v2 using `Event::Extension` |
| `network-tools` | `hya/network` | `native` | `webfetch`/`websearch` + search backends (`config.yml`); disableable for air-gapped operation |
| `extended-tools` + `core-agents` | `hya/plan` | `native` + `static` + `tui` | `hya-plan` agent, `plan_exit` tool, interactive visibility hook, TUI card renderer |
| `goal-loop` | `hya/goal` | `native` + `static` | `completion.rs`, `GoalDriver`, goal skills & `hya-goal-{guide,verifier}` |
| `goal-loop` | `hya/loop` | `native` + `static` | `loop_mode.rs`, `LoopDriver`, loop skills |
| `plan-impl-review` + `extended-tools` | `hya/workflow` | `native` + `static` + `tui` | `hya-workflow` DAG engine, `workflow` tool, `plan-impl-review`, `BundleApi`, `workflows` TUI |
| `channel-tools` + `agent-channels` | `hya/agent-channels` | `native` + `static` + `tui` | Mailbox `NativeService`, `channel_policy`, `channel://` `UriSchemaHandler`, `send`/`list_channel`/`report`/override `wait`, TUI renderers |
| `extended-tools` + `core-agents` | `hya/multi-agent` | `native` + `static` + `tui` | `ResidentSupervisor`/`actor`, `SubagentGovernor`, `terminal_handoff`, `task`/`list_agents`/`search_agent`/`archive`/`wait`, `hya-{task,scout,reviewer}`, `BundleApi`, `SubagentPanes`/`SubagentViewer` TUI |
| `core-agents` | `hya/core-agents` | `static` | Slimmed to `hya-main` + `hya-title` |
| `core-skills`, `core-commands` | `hya/core-skills`, `hya/core-commands` | `static` | Pure v2 static presets |
| `basic-tui-components` | `hya/basic-tui-components` | `js` / `tui` | `projects`/`sessions`/`todos`/`context`/`project_view`/`jobs` (`@hya/tui-sdk` v2) |
| `code-mode` | *(promoted to `hya-bare` core)* | Core | `packages/hya-code-mode` + `crates/hya-app/src/code_mode.rs` |

Legacy `goal-loop` and the four monolithic `*-tools` bundles (`base-tools`, `extended-tools`, `network-tools`, `channel-tools`) are removed.

### 5. Clean-Sheet `hya.v2` Contracts & Breaking-Change Boundaries

All v2 interfaces drop v1 compatibility shims in favor of hard, enforceable boundaries:

1. **Zero `cdylib` linkage to store, engine, or server:** Zero `bundles/builtin/*/native` (or any native `cdylib`) is allowed to link `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, or `tonic`. All runtime, storage, model, event, and HTTP/gRPC interactions pass exclusively through `Arc<dyn HostRuntime>`.
2. **Kernel-exclusive `todo` and `rewrite`:** `todo` and `rewrite` are permanently kernel-exclusive and forbidden from override or aliasing by any bundle, extending [ADR-0035](0035-full-mode-follows-the-kernel.md) F5 into a hard loader/registry invariant.
3. **Clean-sheet Protobuf `hya.v2`:**
   - Delete `proto/hya/v1/` and `crates/hya-sdk-v1`.
   - Standardize on `proto/hya/v2/` (`package hya.v2;`, `/v2/...` HTTP/JSON+SSE+WebSocket and gRPC routes on one port via `hya-server` v2) and `crates/hya-sdk`.
   - Remove the core `Workflow` service and `Session.{ArchiveSubagent, SteerSubagent}` RPCs from core proto. Both Native (`NativeBundleApiHandler`) and JS bundles expose domain endpoints uniformly via `BundleApi`, and domain events stream via generic `StreamEvent` / `ExtensionEvent`.
4. **Event & SQLite v2:**
   - `hya_proto::Event` collapses 25+ domain-specific variants into kernel events plus `Event::Extension { session, bundle, kind, payload }`.
   - `Projection` adds `extensions: BTreeMap<String, Value>`, populated by bundle `ProjectionReducer`s (preserving single-source event-sourcing).
   - `hya-store` moves to `format_version = 2` (`0001_init.sql`, `schema_meta`), replacing domain tables such as `resident_actor_claim` with generic `bundle_state` and `bundle_claim` tables.
   - Opening an unmigrated v1 SQLite database fails fast with a clear error directing the user to run `hya-migration --db <path>`.
5. **`hyabundle` `schema_version: 2` only & `config.yaml` v2:**
   - Remove `schema_version: 1`, `extensions.*`, `exposure.yaml`, and `BaseToolScheme` completely from `crates/hya-bundle`. `hyabundle` accepts `schema_version: 2` only (superseding [ADR-0038](0038-hyabundle-standard-and-independent-ci.md)'s transitional v1 support) and supports `overrides: hya/bare`.
   - `config.yaml` requires `schema_version: 2`, governs built-ins via `bundles.builtin`, and moves all per-bundle domain configuration into `~/.config/hya/bundles/<id>/config.yml`.
6. **`@hya/tui-sdk` v2 (`api_version: 2`) & modular TUI:**
   - `@hya/tui-sdk` advances to `api_version: 2`, providing `tui.bundle_api`, `tui.extension_state.read`, dynamic named panes and slash commands, and host-virtualized `{ type: "transcript", sessionId }` nodes.
   - `packages/hya-tui` speaks `hya.v2`, deletes `legacyPaneLayout.ts` and hardcoded `subagents`/`workflows` domain panes, and lets `bundles/builtin/{multi-agent,workflow,plan,agent-channels,basic-tui-components}` supply all domain UI.

### 6. Standalone `crates/hya-migration` CLI

Because `hya-store`, `hya-bundle`, and `hya-app` reject v1 artifacts at runtime, 100% of v1-to-v2 upgrade logic is isolated in a standalone crate `crates/hya-migration` (binary `hya-migration`, packaged alongside `bin/hya` in release archives):

- **`hya-migration check`:** Inspects `sessions.db`, `config.yaml`, `tui.toml`, and user `.hyabundle` / `.hya/bundles/` directories and reports v1 artifacts requiring migration without writing.
- **`hya-migration run [--dry-run]`:**
  - Computes and writes a **SHA-256 backup manifest** before touching any file.
  - Migrates artifacts via atomic replacement: upgrades `sessions.db` to `format_version = 2` (translating legacy domain events into `Event::Extension` and `resident_actor_claim` into `bundle_claim`/`bundle_state`), upgrades `config.yaml` to `schema_version: 2` (extracting domain blocks to `~/.config/hya/bundles/<id>/config.yml`), migrates `tui.toml`, and rewrites user v1 `.hyabundle` / `.hya/bundles/` manifests to `schema_version: 2`.
- **`hya-migration rollback`:** Restores every artifact from the backup snapshot after verifying each file against the recorded SHA-256 manifest.

## Invariants

1. **No `cdylib` core-infra linkage:** CI and crate boundaries forbid any `bundles/builtin/*/native` library from linking `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, or `tonic`.
2. **Reserved kernel tools:** `todo` and `rewrite` can never be overridden, shadowed, or aliased by any bundle (`hya-bundle`, `hya-tool-loader`, `ToolRegistry`).
3. **Pure `hya-bare` parity:** `hya --bare` boots and runs `exec`, `serve`, and TUI/WebUI with zero `bundles/builtin/` loaded, using only the 3-primitive kernel, $(m, g, w)$ Context, standard `read`/`write`/`edit`/`bash`/`grep`, `todo`/`rewrite`, `skill`, `code-mode` (without `agent()`), and standard `AGENTS.md`/`MCP`/`plugins` adapters.
4. **Single-version wire and storage truth:** Runtime crates speak only `hya.v2`, `format_version = 2`, `hyabundle` `schema_version: 2`, `config.yaml` `schema_version: 2`, and `@hya/tui-sdk` `api_version: 2`; v1 artifacts are handled exclusively by `hya-migration`.
5. **Reversible offline migration:** Every `hya-migration run` produces a SHA-256-verified backup manifest enabling deterministic `hya-migration rollback`.

## Consequences

- **Clean modular boundary:** Core crates (`hya-core`, `hya-engine`, `hya-server`, `hya-store`, `hya-proto`) shrink to kernel + single-agent runtime + generic extension hosting, while domain features evolve independently in `bundles/builtin/` (native, static, and `@hya/tui-sdk` v2).
- **Safe opt-out and air-gapped profiles:** Disabling `hya/hashline` restores standard file primitives cleanly; disabling `hya/network` removes outbound web access; `--bare` eliminates all built-in bundle overhead across all surfaces.
- **Breaking cut requires explicit migration:** Existing v1 databases, configs, TUI presets, and user bundles will not load under v2 `hya` until upgraded with `hya-migration run`.
- **Cross-ADR alignment:** Preserves the kernel primitives and $(m, g, w)$ invariants of [ADR-0033](0033-three-primitive-kernel.md)–[ADR-0035](0035-full-mode-follows-the-kernel.md), and supersedes [ADR-0038](0038-hyabundle-standard-and-independent-ci.md)'s transitional v1 manifest compatibility, `exposure.yaml` usage, and 13-bundle split while implementing its reserved `abi_version: 2` as `crates/hya-runtime-abi`.

## Alternatives rejected

1. **Dual v1/v2 wire and storage shims in runtime crates:** Keeping `proto/hya/v1`, `hya-sdk-v1`, 25+ `Event` variants, and inline SQLite auto-migration inside `hya-server` and `hya-store` would preserve the coupling this refactor eliminates and double the test surface.
2. **Allowing native `cdylib` bundles to link `hya-store` or `hya-engine` directly:** Direct linkage leaks internal database schemas, async executors, and Tokio/Axum/Tonic ABI details across dynamic-library boundaries; `Arc<dyn HostRuntime>` enforces strict host mediation.
3. **Allowing bundles to override `todo` or `rewrite`:** As established in [ADR-0035](0035-full-mode-follows-the-kernel.md), $g$ (`todo`) and destructive context compaction (`rewrite`) are foundational kernel invariants; letting bundles replace them breaks deterministic `View` replay.
4. **Shipping `hashline` inside `hya-bare` or leaving `code-mode` outside `hya-bare`:** `hashline` is an opinionated editing protocol that belongs in an overridable bundle (`overrides: hya/bare`), whereas `code-mode` is lightweight core tooling that makes `hya-bare` complete on its own.

See [ADR-0033](0033-three-primitive-kernel.md), [ADR-0034](0034-context-is-m-g-and-w.md), [ADR-0035](0035-full-mode-follows-the-kernel.md), [ADR-0038](0038-hyabundle-standard-and-independent-ci.md), [Kernel & Context Engine](../architecture/kernel-and-context.md), and [`hyabundle` Specification](../extend/hyabundle-spec.md).
