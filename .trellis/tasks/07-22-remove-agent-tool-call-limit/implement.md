# Implementation Plan

## 1. Preflight

- Recheck `git status`, current version, and the absence of `docs/changes/CHANGELOG_0.33.27.md`.
- Preserve unrelated untracked Trellis task directories and incorporate any concurrent edits rather than overwriting them.

## 2. RED Regression

- Add one `turn_loop.rs` test with 26 tool-bearing scripts plus an explicit final stop script.
- Assert `FinishReason::Stop` and the unique final assistant text.
- Run the exact test and accept RED only when the legacy guard returns `FinishReason::Error` before consuming the final response:

```sh
cargo test -p hya-core --test turn_loop turn_continues_past_twenty_five_tool_rounds -- --exact
```

## 3. Minimum Runtime Change

- Delete `MAX_TOOL_ROUNDS` from `crates/hya-core/src/engine/turn.rs`.
- Delete only the `rounds >= MAX_TOOL_ROUNDS` synthetic text/error/return branch.
- Retain the rounds counter, increment, cancellation path, normal provider completion, tool/provider errors, token accounting, and step events.

## 4. GREEN Checks

```sh
cargo test -p hya-core --test turn_loop turn_continues_past_twenty_five_tool_rounds -- --exact
cargo test -p hya-core --test turn_loop
```

## 5. Release Metadata

- Move the current root `CHANGELOG.md` to `docs/changes/CHANGELOG_0.33.27.md` without changing its contents.
- Set `[workspace.package].version` to `0.33.28`.
- Write root `CHANGELOG.md` with only the `0.33.28` removal note.
- Let Cargo refresh `Cargo.lock`; reject unrelated dependency updates.

## 6. Verification Gate

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p hya --bin hya
git diff --check
```

- Review `git status`, the full diff, and the staged diff.
- Confirm only the runtime, regression test, release metadata, generated lockfile changes, and this task's Trellis artifacts are included.

## 7. Delivery

- After every gate passes, create and push one atomic semantic commit without a release tag.
- If any required gate fails, do not commit or push; record and report the blocker.
