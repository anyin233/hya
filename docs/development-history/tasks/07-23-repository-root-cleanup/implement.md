# Repository root cleanup implementation plan

1. Capture the baseline: Git status, tracked root inventory, Cargo metadata,
   registered worktrees, and target top-level inventory.
2. Load the applicable backend/frontend and shared development guidelines.
3. Move shared fixtures to `tests/fixtures/`; update Rust test paths and run
   their focused tests.
4. Move `xtask` to `crates/xtask/`; update workspace membership and live
   documentation/agent guidance; verify Cargo metadata and the tool command.
5. Move the logo to `docs/assets/hya-icon.png`; update the README, generator,
   and provenance docs; regenerate and inspect the generated files.
6. Remove each registered non-main worktree with `git worktree remove --force`;
   prune metadata and verify only the root worktree remains.
7. Run targeted and full quality gates. Do not include pre-existing untracked
   Trellis tasks in the diff.
8. Clean Cargo artifacts with `cargo clean`, then build the whole workspace in
   debug and release mode to retain one current artifact set for each profile.
9. Scan for stale active references, inspect the final diff, update applicable
   project knowledge if a durable convention was learned, and commit the
   verified atomic refactor.

## Validation

- `cargo metadata --no-deps --format-version 1`
- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo build --workspace`
- `cargo build --workspace --release`
- `cargo run -p xtask -- sync-compat --help`
- `bun run typecheck` and `bun test` in `packages/hya-tui-ts`
- `bun run typecheck` and `bun test` in `crates/hya-plugin-compat/adapter`
- `bash tests/install_script.sh` and `bash tests/claude_isolated.sh`
- logo generator run plus tracked-path reference scan
