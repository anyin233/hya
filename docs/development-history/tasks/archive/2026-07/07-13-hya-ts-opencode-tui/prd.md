# Migrate OpenCode TUI to hya-ts

## Goal

Create a repository-owned TypeScript terminal frontend named `hya-tui-ts` from
OpenCode's TUI, connect it exclusively to hya's backend, and expose the combined
experience through a new `hya-ts` executable.

## Background

- The approved upstream baseline is OpenCode `1.17.9`, commit
  `cf31029350820c6bfc0fbd0e052a79a067ee6116`, from the local source checkout at
  `/chivier-disk/yanweiye/Projects/opencode-frontent-rs/opencode-origin`.
- The upstream TUI is a Bun/TypeScript package built on OpenTUI and SolidJS.
- OpenCode is MIT licensed with `Copyright (c) 2025 opencode`; copied or
  substantially derived portions must retain the complete notice.
- hya already exposes a broad OpenCode-compatible HTTP/SSE API through
  `hya-server`.
- The current `hya` executable launches the Rust `hya-tui` frontend.

## Requirements

- R1: Add a private TypeScript package named `hya-tui-ts` containing the
  OpenCode TUI runtime needed by hya, its required frontend assets, and no
  OpenCode backend, provider runtime, server, or unrelated application code.
- R2: Add an executable named `hya-ts` that starts an owned `hya-backend` by
  default, can attach to an explicitly supplied hya server, and launches
  `hya-tui-ts` for the selected project directory.
- R3: Keep the existing `hya` command and Rust TUI unchanged and available as
  the rollback baseline.
- R4: Use hya's backend for session lifecycle, prompt submission, streamed
  events and tool activity, abort, permissions, questions, models, agents,
  commands, files, MCP, LSP, and formatter status.
- R5: Fix only API gaps demonstrated by the migrated TUI's real SDK requests or
  events. Do not add a TypeScript backend or a parallel projection model.
- R6: Remove OpenCode-backend-only controls that hya cannot honor, including
  OpenCode update, Console/organization, remote workspace, sharing, and dynamic
  TUI plugin installation paths. Do not retain disabled placeholders.
- R7: Preserve static built-in TUI components whose hya contracts work, without
  importing OpenCode's external plugin loader or package manager.
- R8: Replace reachable user-visible OpenCode names, logos, titles, links,
  config/state paths, tips, and command labels with hya equivalents. Internal
  SDK package names and compatibility protocol identifiers may remain when they
  are not presented as product branding.
- R9: Include OpenCode's complete MIT license and a provenance notice naming the
  upstream repository, version, commit, imported boundary, and hya changes.
- R10: The first distribution may require system Bun and may install a locked
  `hya-tui-ts` runtime beside the Rust executables.
- R11: Add executable checks for source boundaries, attribution, branding,
  TypeScript correctness, launcher ownership, SDK/backend compatibility, and
  the core terminal workflow.
- R12: Update the project version and newest-version changelog according to the
  repository release rules.

## Out Of Scope

- Switching the unqualified `hya` command to the TypeScript frontend.
- Removing or refactoring the Rust TUI.
- Migrating OpenCode's backend, provider runtime, server, worker/RPC transport,
  web/desktop UI, Console, updater, telemetry, or unrelated CLI commands.
- Supporting OpenCode external TUI plugins or OpenCode config/state directories.
- Producing a self-contained Bun-free executable or adding non-Linux release
  targets.
- Publishing a release or creating a release tag.
- Adding automated upstream-sync infrastructure before a second sync is needed.

## Acceptance Criteria

- [ ] AC1: `hya-ts` launches `hya-tui-ts` with Bun, scopes it to the selected
      project directory, and either owns a spawned hya backend or attaches to a
      supplied server without terminating that server.
- [ ] AC2: A user can create or resume a session, submit a prompt, observe
      streamed assistant/tool activity, and abort a running turn through hya.
- [ ] AC3: Permission and question requests are delivered live and can be
      answered or rejected exactly once from the migrated TUI.
- [ ] AC4: Models, agents, commands, file completion, MCP, LSP, and formatter
      status used by retained screens come from hya's Compat API.
- [ ] AC5: No reachable control promises OpenCode-only update, Console,
      sharing, remote workspace, or dynamic TUI plugin behavior.
- [ ] AC6: Reachable terminal surfaces identify the product as `hya`; automated
      audit reports no unintended user-visible `OpenCode`, `opencode`, or `OC`
      branding.
- [ ] AC7: The installed and release-package layouts contain the full upstream
      MIT notice and pinned provenance record.
- [ ] AC8: A dependency/source-boundary check proves that no OpenCode backend,
      server, provider runtime, worker, updater, or Console module is included.
- [ ] AC9: Existing `hya` behavior remains unchanged and its tests stay green.
- [ ] AC10: Bun install/typecheck/tests/build, focused Compat integration tests,
      Rust formatting/clippy/workspace tests, installer smoke, and local
      executable builds pass.
- [ ] AC11: Workspace and TypeScript package versions are `0.33.0`, the previous
      `0.32.4` changelog is archived, and root `CHANGELOG.md` contains only the
      `0.33.0` notes.
