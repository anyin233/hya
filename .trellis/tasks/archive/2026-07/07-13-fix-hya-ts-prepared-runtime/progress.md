# Progress

## 2026-07-13

- Reproduced the pushed `0.33.0` installed runtime failure with Bun 1.3.14:
  `Cannot find module './server.js'` from the retained SDK v2 barrel.
- Confirmed the installed unit omitted both `bunfig.toml` and `tsconfig.json`.
- Chosen seams: a real staged SDK prune/import test and the existing isolated
  installer fixture.
- RED: `bun test test/runtime-boundary.test.ts` failed because the pruned v2
  barrel could not import its deleted `server.js`; `bash tests/install_script.sh`
  failed because `bunfig.toml` and `tsconfig.json` were not staged.
- GREEN: pruning now remaps `./v2` to the retained `./v2/client` export,
  atomically replaces the SDK manifest, removes legacy/server/process barrels,
  and self-smokes the client import. The focused Bun test passed (1 test, 18
  assertions), and the installer transaction test passed with both config files.
- Replaced the PTY smoke's fixed delay with a bounded wait for the rendered
  provider response; five consecutive reruns passed.
- Bumped Cargo and `hya-tui-ts` to `0.33.1`, archived the exact `0.33.0`
  changelog, and made root `CHANGELOG.md` describe only `0.33.1`.
- The first `cargo test --workspace` attempt stopped while linking with
  `No space left on device (os error 28)`. Removed the ignored
  `target/debug/incremental` cache (146 GB) and reran with
  `CARGO_INCREMENTAL=0`; the complete workspace test suite passed.
- Final frontend gates passed: frozen Bun install, typecheck, all 9 tests with
  2,060 assertions, and the 166-module production build. The prepared-runtime
  regression passed three consecutive runs and the PTY smoke passed five.
- Final packaging gates passed: installer transaction/rollback test, shell
  syntax, workflow YAML parsing, and syntax checks for all 11 CI plus 7 release
  shell blocks. `actionlint` is unavailable in this environment.
- Final Rust gates passed: formatting, workspace clippy with warnings denied,
  the complete workspace test suite, locked metadata, and locked local builds
  of `hya`, `hya-backend`, and `hya-ts`.
- Version/changelog alignment, task context validation, and `git diff --check`
  passed. Independent read-only review found three verification gaps; the
  release artifact assertions and two test time budgets were corrected, and
  the follow-up review reported no actionable findings.
- Pushed atomic work commits `babd7fb7` (`test: wait for rendered PTY response`)
  and `71e23bdc` (`fix: repair prepared TypeScript runtime`) to `origin/main`.
