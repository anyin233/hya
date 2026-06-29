<!-- TRELLIS:START -->
# Trellis Instructions

These instructions are for AI assistants working in this project.

This project is managed by Trellis. The working knowledge you need lives under `.trellis/`:

- `.trellis/workflow.md` — development phases, when to create tasks, skill routing
- `.trellis/spec/` — package- and layer-scoped coding guidelines (read before writing code in a given layer)
- `.trellis/workspace/` — per-developer journals and session traces
- `.trellis/tasks/` — active and archived tasks (PRDs, research, jsonl context)

If a Trellis command is available on your platform (e.g. `/trellis:finish-work`, `/trellis:continue`), prefer it over manual steps. Not every platform exposes every command.

If you're using Codex or another agent-capable tool, additional project-scoped helpers may live in:
- `.agents/skills/` — reusable Trellis skills
- `.codex/agents/` — optional custom subagents

Managed by Trellis. Edits outside this block are preserved; edits inside may be overwritten by a future `trellis update`.

<!-- TRELLIS:END -->

## Release & Changelog Rule

- Before publishing a new version, the local agent must ensure `[workspace.package].version` in `Cargo.toml`, the `vX.Y.Z` release tag, and root `CHANGELOG.md` all describe the same version.
- Root `CHANGELOG.md` must contain only the newest version's changelog because the GitHub release workflow reads it verbatim as the GitHub Release notes.
- When a previous root changelog exists, move it to `docs/changes/CHANGELOG_<version>.md` before writing the new root `CHANGELOG.md`.
- Historical changelog files stay under `docs/changes/`; do not append old release history back into root `CHANGELOG.md`.

## Project Overview

`hya` is a Rust multi-agent coding agent. It is built as an event-sourced
workspace: user prompts, model deltas, tool calls, permissions, token usage, and
session lifecycle changes are appended as `Event`s, then replayed into a
projection for the TUI, HTTP API, and client surfaces.

The main runtime path is:

```text
hya-backend
  -> config/auth/plugin/MCP setup
  -> hya-core::SessionEngine
  -> hya-provider streaming model route
  -> hya-tool builtin, MCP, or plugin tools
  -> hya-store SQLite event log
  -> hya-legacy-tui or hya-server views over the same projection
```

The engine owns stop decisions. Goal mode and loop mode use separate evaluators
or verifiers; workers do not decide that their own objective is done.

## Component Map

| Component | Feature |
| --- | --- |
| `crates/hya-backend` | Umbrella `hya` binary. Launches the interactive TUI by default; also supports `exec`, `-p/--prompt` goal mode, `serve`, `tail-session`, auth/token commands, session listing, JSONL RPC, slash commands, prompt templates, plugin loading, MCP setup, permission policy, and AGENTS/skills discovery. |
| `crates/hya-core` | Agent runtime. Owns `SessionEngine`, turn admission, streaming rounds, shell turns, event bus, prompt construction, compaction, goal/loop drivers, hook dispatch, subagents, team state, worktree/tmux helpers, and session forking. |
| `crates/hya-proto` | Shared wire/domain types. Defines newtyped IDs, tagged `Event`/`Envelope`, messages, parts, roles, model/tool schema types, API DTOs, and the deterministic projection reducer. Keep this dependency-light so UI/client crates can reuse it cheaply. |
| `crates/hya-provider` | Model provider abstraction. Normalizes OpenAI-compatible, Anthropic, Google, dev, and fake providers into one streamed `Event` model; handles protocol encoding/decoding, provider routing, capability metadata, reasoning effort, and preflight checks for tool-capable routes. |
| `crates/hya-tool` | Tool and permission plane. Provides the `Tool` trait, registry, allow/ask/deny rules, interaction/question requests, spawn/todo/skill/websearch/LSP planes, and builtins for read/write/edit/apply-patch, shell, web fetch/search, task/todo, and invalid-tool handling. |
| `crates/hya-store` | Persistence. Stores events and token ledger entries in SQLite, runs migrations, lists/deletes sessions, replays event logs, and folds projections on read through `hya-proto::Projection`. |
| `crates/hya-server` | Axum HTTP and SSE surface over `hya-core`. Serves native session/prompt/command/shell/events/stream APIs plus OpenCode-compatible session, event, file, project, VCS, MCP, PTY, TUI, permission, and question endpoints. |
| `crates/hya-client` | Small typed `reqwest` client for the server API: create sessions, send prompts, and read events. |
| `crates/hya-legacy-tui` | Pure ratatui rendering layer. Holds projected app state, input state, theme, transcript rendering, status line, overlays, pickers, permission prompts, and question prompts. Terminal I/O and the event loop stay in `hya-backend`. |
| `crates/hya-mcp` | MCP support. Implements the MCP protocol/client/manager and bridges MCP tools into `hya-tool` with namespaced `mcp__server__tool` names and permission checks. |
| `crates/hya-plugin` | Out-of-process plugin host. Owns the JSON-RPC stdio protocol, plugin client/host, manifest/config loading, command/tool dispatch, hook dispatcher bridge, permission bridge, and plugin-backed tool adapter. |
| `crates/hya-plugin-opencode` | OpenCode plugin compatibility. Rust crate pins the supported OpenCode package versions; the Bun adapter discovers OpenCode plugin config, loads plugins, translates hook/tool/event methods, and exposes the adapter runtime over JSON-RPC. |
| `crates/hya-plugin-example` | Minimal plugin binary used as a concrete fixture/example for host and transport behavior. |
| `xtask` | Dev-tooling entry point. Currently a small scaffold for future workspace maintenance commands. |
| `.trellis` | Project workflow knowledge: task lifecycle, package/layer specs, session journals, and task artifacts. Read the relevant `.trellis/spec/**/index.md` before changing code in that layer. |
| `docs` | Supplemental project notes such as OpenCode parity and follow-up work. |
| `DESIGN.md` | TUI design system: terminal-first visual rules, theme tokens, layout, transcript/input/overlay behavior. Read before touching TUI rendering. |

## Change Guidance

- Rust workspace uses edition 2024 and `rust-version = "1.91"`.
- Library crates deny `unwrap_used` and `expect_used`; keep panic paths out of
  library code and use typed errors where the crate already has one.
- Preserve the event-sourced architecture: append events, replay with the shared
  projection, and avoid parallel read-model logic that can drift from replay.
- Keep `hya-proto` free of heavy runtime dependencies.
- Keep `hya-legacy-tui` as pure rendering/state; terminal I/O belongs in `hya-backend`.
- Prefer existing planes (`PermissionPlane`, `InteractionPlane`, `SpawnerPlane`,
  `TodoPlane`, `SkillPlane`, `WebSearchPlane`, `LspPlane`) over adding another
  cross-cutting runtime channel.
- For TypeScript adapter work, keep it under
  `crates/hya-plugin-opencode/adapter` and use the existing Bun/TypeScript
  scripts instead of adding another JS toolchain.

## Verification

For Rust changes, run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For OpenCode adapter changes, also run from
`crates/hya-plugin-opencode/adapter`:

```sh
bun run typecheck
bun test
```
