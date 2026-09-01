# hya Documentation

hya is an event-sourced, terminal-first coding agent. Rust owns the runtime,
launcher, server, and persistence boundaries; `packages/hya-tui-ts` owns the
interactive TypeScript/OpenTUI frontend. Workflow compilation, durable
execution, package models, and the HTTP/SSE/Compat surfaces are documented
separately below.

This documentation is split into user-facing guides and maintainer-facing
architecture notes.

## Reading Paths

If you want to run hya:

1. [Getting Started](getting-started.md)
2. [Configuration](configuration.md)
3. [CLI Reference](cli.md)
4. [TUI Keybindings](tui-keybindings.md) — slash commands, leader chords, and the command palette
5. [TUI Reference](tui-reference.md) — screens, transcript, dialogs, and prompt behavior
6. [Skills](skills.md) — discovery, skill tool, and authoring
7. [Workflows](workflows.md) — user-authored stage DAGs over subagent teams
8. [Troubleshooting](troubleshooting.md)

If you want to compare hya with adjacent coding agents:

1. [hya, Pi, and Compat Feature Comparison](hya-pi-compat-comparison.md)

If you want to package a public AgentBundle or WorkflowBundle:

1. Run `hya bundle info -f example.hyabundle`, then
   `hya bundle install example.hyabundle`.
2. Read [AgentBundle Authoring](agent-bundle-authoring.md) for singular Agent
   payloads, or [Workflows](workflows.md#packaging-a-workflowbundle) for one
   Workflow plus its exact reachable Agent closure. Sources use exactly one
   root form: `bundle.yaml` for both kinds, or `bundle.hya.md` only for an
   AgentBundle body prompt. Start from the
   [single-file static example](examples/bundle.hya.md), the
   [transient Bun example](examples/bun-transient/), the
   [resident Bun example](examples/bun-resident/), the
   [disjoint Bun example](examples/bun-disjoint/), or the
   [directory `bundle.yaml` layout](../crates/hya-bundle/tests/fixtures/directory/bundle.yaml),
   and use the [bundle CLI reference](cli.md#bundle-commands). Built-in Agents
   are compiled in and cannot be shadowed by either package kind.

If you want the independent self-update path (0.34.13+):

1. Read [Secure self-update](self-update.md).
2. Run the local dry-run under [examples/self-update](examples/self-update/).
3. Remember: signatures alone never activate; production needs explicit owner
   authorization, and `install.sh` remains break-glass recovery.

If you want to understand the codebase:

1. [Project Structure](project-structure.md)
2. [Architecture Overview](architecture/overview.md)
3. [Runtime](architecture/runtime.md)
4. [Admission and Governor](architecture/admission-and-governor.md)
5. [Event Model](architecture/event-model.md)
6. [Providers](architecture/providers.md)
7. [Tools and Permissions](architecture/tools-and-permissions.md)
8. [Agent tool surface](architecture/agent-tool-surface.md)
9. [Storage](architecture/storage.md)
10. [Server and Client](architecture/server-client.md)
11. [Plugin protocol](plugin-protocol.md)
12. [TUI](architecture/tui.md)
13. [Development](development.md)
14. [Testing](testing/README.md) (process E2E, agent matrix, CI snippet)

## Docs Map

| Page | Purpose |
| --- | --- |
| [Getting Started](getting-started.md) | Build and run the TUI, a headless prompt, a goal run, and the server. |
| [Configuration](configuration.md) | Explain hya config, provider/auth resolution, MCP, plugins, formatter, and prompt-command discovery. |
| [CLI Reference](cli.md) | Document shipped `hya` / `hya-backend` commands, flags, exit codes, and a TUI slash-command overview. |
| [TUI Keybindings](tui-keybindings.md) | Full keyboard shortcuts, slash commands, leader chords, and which-key. |
| [TUI Reference](tui-reference.md) | User-facing screens, transcript, dialogs, prompt, and overlays. |
| [Skills](skills.md) | Skill discovery paths, skill tool, and authoring. |
| [Workflows](workflows.md) | Workflow DAGs, governance, discovery, CLI/tool execution, and WorkflowBundle packaging. |
| [Plugin protocol](plugin-protocol.md) | Native stdio JSON-RPC ABI for out-of-process plugins. |
| [Compat plugins](compat-plugins.md) | Bun adapter for OpenCode/Compat JS plugins over the native host. |
| [AgentBundle Authoring](agent-bundle-authoring.md) | Package, inspect, install, list, describe, and uninstall singular public AgentBundles, including static and Bun-sidecar forms. |
| [Secure self-update](self-update.md) | Independent `hya-updater` TCB: signed metadata, local package stage, smoke, owner-gated activation, break-glass installer. |
| [Project Structure](project-structure.md) | Map repository paths, crates, modules, tests, and data flow. |
| [Architecture Overview](architecture/overview.md) | Explain the crate boundary model and end-to-end request path. |
| [Runtime](architecture/runtime.md) | Explain `SessionEngine`, turn execution, goal mode, loop mode, teams, and worktrees. |
| [Admission and Governor](architecture/admission-and-governor.md) | Spawn admission journal, subagent governor budgets, and depth limits. |
| [Event Model](architecture/event-model.md) | Explain canonical events, envelopes, messages, parts, ids, and projections. |
| [Providers](architecture/providers.md) | Explain provider routing, OpenAI-compatible, Anthropic, Responses, and Google protocols, SSE decoding, and fallback providers. |
| [Tools and Permissions](architecture/tools-and-permissions.md) | Explain builtin tools, permission rules, ask flows, and output limits. |
| [Agent tool surface](architecture/agent-tool-surface.md) | Canonical tool registry surface, aliases, resource views, and agent-facing tool contracts. |
| [Compat parity](compat-parity.md) | Feature-by-feature Compat baseline status for tools, APIs, CLI, and remaining gaps. |
| [Storage](architecture/storage.md) | Explain SQLite persistence, replay, projections, and token ledger behavior. |
| [Server and Client](architecture/server-client.md) | Explain native HTTP/SSE, Compat-compatible route groups, and the typed client crate. |
| [TUI](architecture/tui.md) | Explain the canonical launcher, Bun/OpenTUI frontend, and backend SDK boundary. |
| [TypeScript TUI package](../packages/hya-tui-ts/README.md) | Frontend-only package: launch with `--url`, layout, env flags, and re-sync rules. See also [scripts](../packages/hya-tui-ts/scripts/README.md) and [tests](../packages/hya-tui-ts/test/README.md). |
| [hya, Pi, and Compat Feature Comparison](hya-pi-compat-comparison.md) | Compare hya with upstream stock Pi and current Compat across tools, providers, agents, TUI, plugins, skills, and MCP. |
| [Development](development.md) | Explain build, lint, test, crate-change, and doc-update workflow. |
| [Testing](testing/README.md) | Track I/P/T testing model, process E2E harness, agent matrix, CI snippet. Package-level TUI tests: [hya-tui-ts test README](../packages/hya-tui-ts/test/README.md). |
| [Agent feature matrix](testing/agent-matrix.md) | PR-matrix scenario IDs for tools, permissions, MCP, subagents, hyabundle, TUI. |
| [Process E2E harness](testing/process-e2e.md) | How `crates/hya-e2e` scripts FakeLlm and asserts product outcomes. |
| [Troubleshooting](troubleshooting.md) | Collect common local, provider, terminal, permission, and server issues. |

## Source Entrypoints

- Workspace manifest: [`../Cargo.toml`](../Cargo.toml)
- Exec shim: [`../crates/hya/src/main.rs`](../crates/hya/src/main.rs)
- Frontend supervisor: [`../crates/hya-ts/src/main.rs`](../crates/hya-ts/src/main.rs)
- Backend CLI/runtime: [`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs)
- Workflow compiler and normalized plans: [`../crates/hya-workflow/src/lib.rs`](../crates/hya-workflow/src/lib.rs)
- Core Workflow execution: [`../crates/hya-core/src/workflow/mod.rs`](../crates/hya-core/src/workflow/mod.rs)
- Workflow control/admission: [`../crates/hya-app/src/workflow_control.rs`](../crates/hya-app/src/workflow_control.rs)
- Bundle models/catalog: [`../crates/hya-bundle/src/lib.rs`](../crates/hya-bundle/src/lib.rs)
- Protocol types: [`../crates/hya-proto/src/lib.rs`](../crates/hya-proto/src/lib.rs)
- Providers: [`../crates/hya-provider/src/lib.rs`](../crates/hya-provider/src/lib.rs)
- Tools: [`../crates/hya-tool/src/lib.rs`](../crates/hya-tool/src/lib.rs)
- MCP: [`../crates/hya-mcp/src/lib.rs`](../crates/hya-mcp/src/lib.rs)
- Plugin host: [`../crates/hya-plugin/src/lib.rs`](../crates/hya-plugin/src/lib.rs)
- Compat adapter: [`../crates/hya-plugin-compat/README.md`](../crates/hya-plugin-compat/README.md)
- Store: [`../crates/hya-store/src/lib.rs`](../crates/hya-store/src/lib.rs)
- Server/routes: [`../crates/hya-server/src/lib.rs`](../crates/hya-server/src/lib.rs)
- Native in-process transport: [`../crates/hya-native/src/transport.rs`](../crates/hya-native/src/transport.rs)
- SDK/client bridge: [`../crates/hya-sdk/src/lib.rs`](../crates/hya-sdk/src/lib.rs)
- Self-update TCB: [`../crates/hya-updater`](../crates/hya-updater)
- TUI application: [`../packages/hya-tui-ts/src/main.tsx`](../packages/hya-tui-ts/src/main.tsx)
- TUI package docs: [`../packages/hya-tui-ts/README.md`](../packages/hya-tui-ts/README.md)
