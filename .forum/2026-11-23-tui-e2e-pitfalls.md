# Why does the TUI terminal suite fail or test old code on a fresh checkout?

Four causes came up while working on the TUI structure presets (p2):

- `packages/hya-tui-sdk` and `crates/hya-plugin-bun/adapter` need their own
  `bun install --frozen-lockfile`. Without them the `hya/basic-tui-components`
  extension host exits during `host/load`, and every spec that reads the
  Projects, Sessions, Todos, or Context panes fails (33 at once).
- `packages/hya-tui/src/main.ts` loads `packages/hya-tui/dist/app.js` when it
  exists. After `bun build.ts`, specs run the precompiled TUI, not the source;
  delete `dist/` before running them.
- `hya-tui-disk-inspector.spec.ts` spawns `target/debug/xtask`; build it
  (`cargo test -p xtask` does) or the spec fails at once.
- On a shared machine, `cargo test` with the default job count next to the
  terminal suite with 12 workers got the test run killed. Use
  `CARGO_BUILD_JOBS=4 cargo test -j 4 … -- --test-threads=4`,
  `HYA_TUI_EXP_WORKERS=3`, and do not run both at once.

[Running the tests](../docs/tui-web.md#running-the-tests)
