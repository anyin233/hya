# Remove retained Rust TUI crates

We delete the retained Rust interactive TUI crates (`crates/hya-tui`,
`crates/hya-tui-lib`) and the orphan TS-vs-Rust parity harness
(`crates/hya-parity`). The TypeScript package `packages/hya-tui-ts` is the only
interactive terminal UI implementation. No shipped binary had launched the Rust
renderer after the default-TS cutover; keeping the crates invited dual-frontend
drift and extra build surface.

## Consequences

- New interactive UI work belongs only in `packages/hya-tui-ts`.
- Do not reintroduce a Rust TUI crate, ratatui frontend, or backend-owned
  terminal renderer without a new ADR.
- Compat HTTP `/tui/*` control endpoints in `hya-server` remain; they are not a
  Rust TUI implementation.
- The product design system in `DESIGN.md` still applies to the TS frontend.
