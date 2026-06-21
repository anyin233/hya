# Ratatui TUI redesign implementation plan

## Validation

Run before code changes:

```sh
cargo build --workspace
cargo test -p yaca-tui
```

Run before completion:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Steps

1. Add failing render tests for wide sidebar, narrow no-sidebar behavior, message rails, and tool status rows in `crates/yaca-tui/tests/tui_render.rs`.
2. Implement `theme.rs`, `layout.rs`, `view_model.rs`, and `widgets.rs` with minimal public surface.
3. Refactor `lib.rs` to use the new modules while preserving `AppState`, `GoalView`, `LoopView`, and `draw`.
4. Run focused `cargo test -p yaca-tui` after each behavior.
5. Run full workspace verification.
6. Update this task if implementation discovers a reusable TUI convention.

## Rollback

If the new layout fails late, revert `crates/yaca-tui` to the previous single-file rendering and keep only the new tests that describe still-desired behavior. No data migrations or external service changes are involved.
