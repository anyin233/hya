# 0.34.0

- Removed the retained Rust interactive TUI crates (`hya-tui`, `hya-tui-lib`) and
  the unused TS-vs-Rust parity harness (`hya-parity`).
- The TypeScript package `packages/hya-tui-ts` is the sole interactive terminal
  UI implementation; launch path remains `hya` → `hya-ts` → prepared runtime.
