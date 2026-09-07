# Progress

## Completed cleanup

- Moved shared JSON fixtures from the repository root to `tests/fixtures/` and
  updated all live Rust test paths.
- Moved the developer workspace tool from `xtask/` to `crates/xtask/` and
  simplified the workspace member glob to `crates/*`.
- Moved the logo asset to `docs/assets/hya-icon.png`, updated its consumers,
  and regenerated the TypeScript art provenance headers.
- Removed every registered non-main worktree with force, pruned stale
  registrations, and removed the now-empty `.worktrees/` directory.
- Ran a final `cargo clean` (removed 34,034 files / 39.8 GiB), then rebuilt
  the entire workspace once in each retained profile: `cargo build --workspace`
  and `cargo build --workspace --release`.
- Corrected the README workspace-version metadata from `0.33.29` to the
  current workspace version, `0.33.40`.

## Verification completed

- `cargo metadata --no-deps --format-version 1`
- `cargo test -p hya-sdk -p hya-tui -p xtask`
- `cargo test --workspace -q`
- `cargo run -p xtask -- sync-compat --help`
- `cargo clippy -p hya-sdk -p xtask --all-targets -- -D warnings`
- `bun run typecheck` and `bun test` in `packages/hya-tui-ts`
- `bun run typecheck` in `crates/hya-plugin-compat/adapter`
- Isolated adapter suite: `env -u HOME -u XDG_CONFIG_HOME -u XDG_STATE_HOME bun test`
  outside the restricted sandbox (62 pass, 0 fail)
- `bash tests/claude_isolated.sh`, `bash tests/install_script.sh`, and
  `bash scripts/verify-no-http.sh`
- Targeted `rustfmt --check`, `git diff --check`, stale-path scan, and final
  root/worktree inventory scan.

## Environment and baseline notes

- The restricted sandbox blocks Bun child-process pipe writes (`send(2)` /
  `EPERM`), so the adapter suite must run outside it. The unrestricted suite
  must also omit user configuration variables to avoid unrelated local Compat
  plugins. Neither issue is caused by this task.
- `cargo fmt --all --check` still reports formatting only in the untouched
  `crates/hya-core/tests/subagent.rs` baseline. Full-workspace Clippy likewise
  has a pre-existing `large_enum_variant` diagnostic in untouched
  `crates/hya-tui/src/app.rs`; the touched `hya-sdk` and `xtask` packages pass
  strict Clippy.
- No durable Trellis coding-spec convention was learned beyond the updated
  project-structure documentation, so no `.trellis/spec/` change is needed.
