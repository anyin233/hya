# Implementation Plan: Ignore unused batch task IDs

## Scope

One test, one branch-local source move, and the required `0.33.19` release metadata. Provider, schema, resolver, runtime, and unrelated task files stay unchanged.

## Phase 0: Approval And Baseline Gate

1. Review `prd.md`, `design.md`, this plan, and both context manifests.
2. Obtain explicit user approval before activation.
3. Run the Trellis artifact validator, then start the task with `task.py start`.
4. Capture `git status --short` and inspect every target file for concurrent edits.
5. Stop if a target changed incompatibly; otherwise preserve all baseline paths outside this task.

Completion: task status is `in_progress`, target files are current, and unrelated baseline work is recorded.

## Phase 1: Establish RED

Edit only `crates/hya-tool/tests/task.rs`.

1. Add `task_batch_ignores_invalid_top_level_task_id` beside the existing task-ID tests.
2. Reuse `TaskTool`, `SpawnerPlane::new()`, `test_context`, and the existing spawned-execution pattern.
3. Pass required top-level fields, `task_id: "new"`, and two `explore` members.
4. Require a received spawn request; assert two members and `member.task_id.is_none()` for all members.
5. Send two successful outcomes and assert execution succeeds.

Run:

```sh
cargo test -p hya-tool --test task task_batch_ignores_invalid_top_level_task_id -- --exact
```

Accepted RED: the test fails because the call returns `invalid task_id` and the batch never reaches the spawner. Compile, permission, timeout, or fixture failures are not valid RED.

## Phase 2: Minimum GREEN

Edit only `crates/hya-tool/src/task.rs`.

1. Remove the eager top-level `task_id` parse before member construction.
2. Insert the same parse and `invalid task_id` error mapping at the start of `if args.members.is_empty()`.
3. Leave member mapping, permission checks, background constraints, schema, and spawner dispatch unchanged.

Run:

```sh
cargo test -p hya-tool --test task task_batch_ignores_invalid_top_level_task_id -- --exact
cargo test -p hya-tool --test task
```

Completion: the new test is green, and existing valid/malformed single-member resume tests still pass.

## Phase 3: Release Metadata

Revalidate that the workspace still reports `0.33.18`, root `CHANGELOG.md` still contains that release, and `docs/changes/CHANGELOG_0.33.18.md` is absent.

1. Bump `[workspace.package].version` in `Cargo.toml` to `0.33.19`.
2. Add `docs/changes/CHANGELOG_0.33.18.md` with the old root changelog verbatim.
3. Replace root `CHANGELOG.md` with only `0.33.19` and a concise note about ignored unused batch task IDs.
4. Change the version mirrors in `README.md` and `packages/hya-tui-ts/package.json` to `0.33.19`.
5. Run `cargo check -p hya-tool` to align `Cargo.lock`.
6. Inspect `Cargo.lock`; only workspace package versions may change.

Completion: Cargo metadata, lockfile workspace packages, README, private package, and root changelog all report `0.33.19`; the prior changelog is archived unchanged.

## Phase 4: Verification Gate

Run in order:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --locked -p hya -p hya-backend
target/debug/hya --version
target/debug/hya-backend --version
git diff --check
```

Inspect the complete diff and `git status --short`. Verify that:

- Both binaries report `0.33.19`.
- No dependency versions or checksums changed in `Cargo.lock`.
- No provider, resolver, runtime, unrelated Trellis task, or architecture document changed.
- Only explicit task-owned paths are staged.

Completion: every command passes and the final diff is limited to the files listed in `design.md` plus this task's planning/lifecycle artifacts.

## Phase 5: Finish

1. Run the Trellis finish/quality workflow and record verification results in `progress.md`.
2. Stage only the atomic fix, release metadata, and this task's artifacts.
3. Commit with a one-line semantic message such as `fix(hya-tool): ignore unused batch task ids`.
4. Push only after all required gates pass. Do not tag or publish a release.

## Rollback

Before commit, remove only this task's source, test, and release hunks. After push, use a normal revert commit. No persisted event or database migration requires rollback.
