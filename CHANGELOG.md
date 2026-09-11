# 0.36.27

## Documentation cleanup (no behavior change)

- Track P registry: `crates/hya-e2e/tests/p20_model_catalog_discovery.rs` was
  unregistered, so `cargo xtask matrix-check` failed. It is now scenario `T2.15`,
  and `docs/testing/process-e2e.md` / `docs/testing/agent-matrix.md` cover
  `p01`–`p20`.
- `docs/architecture/`: corrected the `Event` catalog (56 variants, adds
  `session_agent_model_override_set`), `SessionProjection`, `reasoning_start`,
  `member_spawned`, and the strong-id table; the native session-not-found,
  command-response, `CommandRequest`/`ShellRequest`, `AppState`, and
  `/tui/bootstrap` contracts; the `StoreError` and `ToolError` catalogs; `find`'s
  external-directory check; `CompactionConfig.context_fraction`; the engine's
  bound spawn/workflow senders; `Capabilities.max_output`; the launcher's
  `boot.tsx`/`dist` entry resolution; and the vendored IDE lock-file discovery.
  Removed the claim that the TypeScript TUI folds `hya_proto::Projection`.
- Guides: first run creates the starter config without an import prompt, one
  Escape interrupts a turn, discovered catalogs persist in `models.yml.cache`,
  `HYA_COMPACTION_CONTEXT_FRACTION` is documented, and `docs/cli.md` covers
  `hya-updater init-roots` and `apply --trust-roots`. `docs/development.md` now
  matches the real `bun run build` script and points at the package test
  inventory instead of a stale file count. `docs/project-structure.md` adds
  `packages/`, migration `0009`, and `GET /sessions/:id/workflow`.
- `docs/superpowers/` moved to `docs/development-history/superpowers/`: both
  plans still instructed agents to build the removed ratatui TUI and the
  in-process `crates/hya` installer.
- `.agent-docs/`, `.argus_subagents/`, and `.autors/` are untracked and ignored
  as machine-local agent artifacts, matching the existing `.gitignore` policy.
- `CLAUDE.md` no longer titles the project `yaca`; `AGENTS.md` no longer claims
  `.planning/.active_plan` always exists.
