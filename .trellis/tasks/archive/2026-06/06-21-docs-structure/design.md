# Docs Structure Design

## Scope

This task creates project documentation only. Trellis remains an internal
workflow mechanism and must not be described in `docs/`.

## Information Architecture

Use a hybrid docs structure:

```text
docs/
  README.md
  getting-started.md
  configuration.md
  cli.md
  project-structure.md
  architecture/
    overview.md
    runtime.md
    event-model.md
    providers.md
    tools-and-permissions.md
    storage.md
    server-client.md
    tui.md
  development.md
  troubleshooting.md
```

This borrows OpenCode's discoverable topic-page style for user docs and Oh My
Pi's engineering-topic depth for architecture docs, while keeping the tree small
enough for hya's current workspace.

## Content Boundaries

- `docs/README.md`: landing page, reading paths, docs map.
- `getting-started.md`: prerequisites, build, first TUI run, first headless run.
- `configuration.md`: opencode config reuse, `HYA_MODEL`, fallback provider,
  security notes for API keys.
- `cli.md`: command reference for the shipped CLI surface.
- `project-structure.md`: repository map, crate table, source-file guide, data
  flow, tests and migrations.
- `architecture/overview.md`: system overview and crate dependency narrative.
- `architecture/runtime.md`: `SessionEngine`, turn loop, goal mode, loop mode,
  teams/subagents/worktrees.
- `architecture/event-model.md`: ids, events, envelopes, messages, projection.
- `architecture/providers.md`: provider trait, router, OpenAI/Anthropic SSE
  decoding, fake provider fallback.
- `architecture/tools-and-permissions.md`: tool registry, builtins, permission
  decisions and tool-output shape.
- `architecture/storage.md`: SQLite event log, projection cache, token ledger.
- `architecture/server-client.md`: axum API, SSE stream, reqwest client.
- `architecture/tui.md`: ratatui rendering boundary and CLI integration.
- `development.md`: build/test/lint workflow and crate-change guidance.
- `troubleshooting.md`: common config, provider, server, and TUI problems.

## Source Grounding

Claims must be grounded in the current code, especially:

- Root `Cargo.toml` and `README.md`
- `crates/hya-cli/src/main.rs`, `config.rs`, `tui.rs`
- `crates/hya-core/src/*.rs`
- `crates/hya-proto/src/*.rs`
- `crates/hya-provider/src/*.rs`
- `crates/hya-tool/src/*.rs`
- `crates/hya-store/src/*.rs`, `migrations/*.sql`
- `crates/hya-server/src/lib.rs`
- `crates/hya-client/src/lib.rs`
- `crates/hya-render-tui/src/lib.rs`

## Risks

- Documentation may overstate future behavior. Mitigation: phrase current
  shipped behavior precisely and omit unsupported surfaces.
- Docs may accidentally include Trellis internals. Mitigation: scan `docs/` for
  `.trellis` and `Trellis` before finishing.
- Link drift. Mitigation: run a local Markdown link check script or equivalent
  shell validation over relative links.
