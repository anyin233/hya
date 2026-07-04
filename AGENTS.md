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

## Commit Rule

- When the user explicitly asks for commits, create one git commit per atomic change before reporting done; for verified feature work, commit and push the atomic change before reporting done.
- Stage only the files for that atomic change; never sweep in unrelated workspace changes.
- Use one-line semantic commit messages with no agent or AI attribution.
- Do not commit or push feature work until its required TDD test and verification gate have passed.

## Feature Workflow Rule

- For every user-requested feature, follow TDD: add one atomic failing test first, verify it fails for the expected missing behavior, implement the smallest change that passes, then run the required verification gate for the touched area.
- After the implementation is verified, the agent must commit and push the atomic feature change.
- If the feature cannot be verified, do not commit or push; report the blocker and the commands or checks that failed.

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

The main runtime path is:

```text
hya / hya-backend / hya-server
  -> hya-app config/auth/plugin/MCP setup
  -> hya-core::SessionEngine
  -> hya-provider streaming model route
  -> hya-tool builtin, MCP, or plugin tools
  -> hya-store SQLite event log
  -> hya-tui, hya-server, or hya-client views over the same projection
```

The engine owns stop decisions. Goal mode and loop mode use separate evaluators
or verifiers; workers do not decide that their own objective is done.

## Component Map

| Component | Feature |
| --- | --- |
| `crates/hya` | User-facing frontend binary. Runs the current interactive TUI, connects to an in-process/native/HTTP/Compat backend, queues input while connecting, and validates startup resume before navigation. |
| `crates/hya-backend` | Backend umbrella binary. Bare startup still launches the interactive TUI by spawning the current `hya` frontend, but no longer owns a legacy TUI renderer/controller. Also supports `exec`, `-p/--prompt` goal mode, `serve`, `tail-session`, auth/token commands, session listing, JSONL RPC, prompt templates, plugin loading, MCP setup, permission policy, and AGENTS/skills discovery. |
| `crates/hya-tui` | Current ratatui app runtime. Owns terminal setup, crossterm event input, app state, keymaps, panes, screens, widgets, theme, prompt/transcript rendering, status, overlays, permission prompts, and question prompts. |
| `crates/hya-tui-lib` | Pure reusable terminal UI primitives: geometry, color, flex layout, overlay/layer validation, component descriptors, and ratatui adapters. |
| `crates/hya-core` | Agent runtime. Owns `SessionEngine`, turn admission, streaming rounds, shell turns, event bus, prompt construction, compaction, goal/loop drivers, hook dispatch, subagents, team state, worktree/tmux helpers, and session forking. |
| `crates/hya-proto` | Shared wire/domain types. Defines newtyped IDs, tagged `Event`/`Envelope`, messages, parts, roles, model/tool schema types, API DTOs, and the deterministic projection reducer. Keep this dependency-light so UI/client crates can reuse it cheaply. |
| `crates/hya-provider` | Model provider abstraction. Normalizes OpenAI-compatible, Anthropic, Google, dev, and fake providers into one streamed `Event` model; handles protocol encoding/decoding, provider routing, capability metadata, reasoning effort, and preflight checks for tool-capable routes. |
| `crates/hya-tool` | Tool and permission plane. Provides the `Tool` trait, registry, allow/ask/deny rules, interaction/question requests, spawn/todo/skill/websearch/LSP planes, and builtins for read/write/edit/apply-patch, shell, web fetch/search, task/todo, and invalid-tool handling. |
| `crates/hya-store` | Persistence. Stores events and token ledger entries in SQLite, runs migrations, lists/deletes sessions, replays event logs, and folds projections on read through `hya-proto::Projection`. |
| `crates/hya-server` | Axum HTTP and SSE surface over `hya-core`. Serves native session/prompt/command/shell/events/stream APIs plus Compat-compatible session, event, file, project, VCS, MCP, PTY, TUI, permission, and question endpoints. |
| `crates/hya-client` | Small typed `reqwest` client for the server API: create sessions, send prompts, and read events. |
| `crates/hya-mcp` | MCP support. Implements the MCP protocol/client/manager and bridges MCP tools into `hya-tool` with namespaced `mcp__server__tool` names and permission checks. |
| `crates/hya-plugin` | Out-of-process plugin host. Owns the JSON-RPC stdio protocol, plugin client/host, manifest/config loading, command/tool dispatch, hook dispatcher bridge, permission bridge, and plugin-backed tool adapter. |
| `crates/hya-plugin-compat` | Compat plugin compatibility. Rust crate pins the supported Compat package versions; the Bun adapter discovers Compat plugin config, loads plugins, translates hook/tool/event methods, and exposes the adapter runtime over JSON-RPC. |
| `crates/hya-plugin-example` | Minimal plugin binary used as a concrete fixture/example for host and transport behavior. |
| `xtask` | Dev-tooling entry point. Currently a small scaffold for future workspace maintenance commands. |
| `.trellis` | Project workflow knowledge: task lifecycle, package/layer specs, session journals, and task artifacts. Read the relevant `.trellis/spec/**/index.md` before changing code in that layer. |
| `docs` | Supplemental project notes such as Compat parity and follow-up work. |
| `DESIGN.md` | TUI design system: terminal-first visual rules, theme tokens, layout, transcript/input/overlay behavior. Read before touching TUI rendering. |

## Change Guidance

- Rust workspace uses edition 2024 and `rust-version = "1.91"`.
- Library crates deny `unwrap_used` and `expect_used`; keep panic paths out of
  library code and use typed errors where the crate already has one.
- Preserve the event-sourced architecture: append events, replay with the shared
  projection, and avoid parallel read-model logic that can drift from replay.
- Keep `hya-proto` free of heavy runtime dependencies.
- Keep `hya-tui-lib` pure and app-neutral. `hya-tui` owns terminal runtime/rendering; do not reintroduce a backend-owned legacy TUI controller or renderer.
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
cargo test --workspace
```

For Compat adapter changes, also run from
`crates/hya-plugin-compat/adapter`:

```sh
bun run typecheck
bun test
```
