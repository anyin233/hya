# Land swarm runtime branch into main

Child 1 of `08-05-e2e-suite-hardening`. Prerequisite for all sibling children.

## Goal

Fast-forward `codex/modular-harness-native-swarm-runtime-refresh` into `main`
and publish it, so that `crates/hya-e2e`, `crates/hya-bundle`, the swarm
runtime, and `docs/testing/` exist on `main` for the four gap-closing children
to build on.

## Why this exists

The E2E suite this task tree is about does not exist on `main`. Parent PRD
decision **D1** selected the full fast-forward over a selective cherry-pick,
because `p11_hyabundle` depends on `crates/hya-bundle`, which `main` lacks.

## Requirements

- R1. Resolve the dirty `main` working tree before merging. At planning time:
  12 tracked paths modified or deleted, 8 untracked paths. Each must be
  committed, stashed, or discarded as a deliberate decision — never silently
  overwritten by the checkout.
- R2. Merge as a fast-forward. `main` is a strict ancestor (0 ahead / 75
  behind), so no merge commit, no history rewrite, and no conflict resolution
  should be required. If git reports a non-fast-forward, stop and re-plan
  rather than forcing.
- R3. Verify the merged `main` against the full quality gate before pushing.
- R4. Push `main` to `origin` only after R3 is green.
- R5. Retire or document the now-redundant worktree at
  `.worktrees/modular-harness-native-swarm-runtime-refresh`, since its branch
  and `main` become identical.

## Constraints

- This is an outward-facing, effectively irreversible publish: `main` and
  `origin/main` are currently in sync, and the merge pushes 75 commits /
  517 files / +91,107 lines to a public GitHub repository
  (`anyin233/hya`). Explicit user confirmation was obtained on 2026-08-05.
- `crates/hya-tool/Cargo.toml` is dirty locally **and** modified by the branch.
  This is the one path where the working-tree decision in R1 can actually
  collide with the checkout.
- The local uncommitted changes to `crates/hya-sdk/`, `crates/xtask/`, and the
  `fixtures/` + `imgs/` deletions are not on the branch; they belong to other
  in-flight tasks and must survive or be intentionally dropped, not lost.
- Vendored `sevenz-rust2-0.20.2` (32 files) enters the tree with this merge.
  It is accepted as-is; auditing vendored code is out of scope here.

## Acceptance criteria

- [ ] `git status` on `main` is clean, or its remaining contents are an
      explicitly recorded decision in this task's notes.
- [ ] `git merge --ff-only codex/modular-harness-native-swarm-runtime-refresh`
      succeeds on `main` with no merge commit.
- [ ] `git rev-list --left-right --count main...codex/modular-harness-native-swarm-runtime-refresh`
      reports `0	0`.
- [ ] `crates/hya-e2e`, `crates/hya-bundle`, `crates/hya-updater`, and
      `docs/testing/` are present on `main`.
- [ ] Quality gate green on merged `main`:
      `cargo fmt --all --check`;
      `cargo clippy --workspace --all-targets -- -D warnings`;
      `cargo build --workspace`;
      `cargo test --workspace --jobs 1 --exclude hya-e2e`;
      `cargo build -p hya-backend --bin hya-backend` then
      `cargo test -p hya-e2e -- --test-threads=1` (19/19 pass);
      `bash scripts/verify-no-http.sh`.
- [ ] Track T green: `bun test` in `packages/hya-tui-ts` after
      `cargo build --locked -p hya -p hya-backend -p hya-ts --bins`.
- [ ] `origin/main` matches local `main` after push.
- [ ] Worktree disposition recorded (removed, or kept with a stated reason).

## Scope change — 2026-08-05, during execution

The step-7 gate found **6 failing tests** on the merged tree (756 pass / 6 fail),
three of which are a **regression** — they passed on pre-merge `main`. Evidence
and per-test detail are in `findings.md`.

The original "Out of scope" rule below said such failures get their own task.
The user was presented with that option and chose instead to **fix the failures
inside this task before pushing**. R6 is therefore added:

- R6. Diagnose and fix the 6 test failures on merged `main`, TDD-style per
  `AGENTS.md`, so the full gate is green before the push in R4. Root cause is
  not yet established: the error is `SpawnError::Unavailable` (not
  `Overloaded`), and `crates/hya-app/src/runtime.rs` has ~15 sites returning it,
  so this is a real debugging effort, not a known fix.

Consequently the third bullet below is superseded for these six failures. The
first two bullets still hold: no unrelated behavioral changes, no re-review of
the 75 commits.

## Out of scope

- Any behavioral change to the code being landed beyond what R6 requires. This
  task moves commits; it does not otherwise author them.
- Reviewing the 75 commits' content. They were developed and reviewed under
  their own task, `07-30-modular-harness-native-swarm-runtime-refresh`.
- ~~Fixing pre-existing failures that the merge merely reveals~~ — superseded by
  R6 for the six failures found in this run. Any *further* failure discovered
  later still gets its own task.

## Rollback

`main` is recoverable to `156d0ad3` (the pre-merge tip, which equals the
merge-base) with `git reset --hard 156d0ad3` while unpushed. After R4's push,
rollback requires a revert commit or a force-push decision by the user; note
this explicitly before pushing.
