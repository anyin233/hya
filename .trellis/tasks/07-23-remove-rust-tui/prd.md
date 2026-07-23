# Remove retained Rust TUI crates

## Goal

Delete the retained Rust TUI implementation so the only interactive terminal UI
source of truth is `packages/hya-tui-ts` (via `hya` → `hya-ts` → Bun).

## Requirements

- Delete `crates/hya-tui`, `crates/hya-tui-lib`, and orphan `crates/hya-parity`.
- Remove workspace pins / Cargo deps for those crates.
- Keep `packages/hya-tui-ts`, `hya-ts` launcher, install/release runtime layout,
  and Compat HTTP `/tui/*` control endpoints.
- Update live agent/docs guidance so agents do not reintroduce a Rust frontend.
- Bump workspace version and rotate root changelog.

## Acceptance Criteria

- [ ] The three crates are gone from the tree.
- [ ] Guard test asserts their absence and no Cargo dep on them.
- [ ] Live docs / `AGENTS.md` describe a single TS frontend only.
- [ ] Version + changelog updated.
- [ ] Rust workspace fmt/clippy/test and entrypoint builds pass.

## Out of Scope

- TypeScript TUI feature work.
- Porting Rust themes or ratatui behavior into TS.
- Historical superpowers plans / archived tasks.
