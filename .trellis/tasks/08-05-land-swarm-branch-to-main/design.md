# Design — Land swarm runtime branch into main

## Shape of the change

This is a history-movement task, not a code-authoring task. The only technical
design questions are: what happens to the dirty working tree, and how we prove
the merged `main` is sound before it becomes public.

## Merge mechanics

```
merge-base(main, codex/…) = 156d0ad3 = main
git rev-list --left-right --count main...codex/…  →  0	75
```

`main` is a strict ancestor of the branch. Therefore:

- `git merge --ff-only` moves the `main` ref forward. No merge commit, no
  conflict resolution, no history rewrite.
- Content-wise the result is byte-identical to the branch tip, `3c07de55`.
- If git ever reports non-fast-forward, a premise has changed (someone pushed to
  `main`). **Stop and re-plan** — do not fall back to a merge commit or a force.

## Working-tree collision analysis

The `main` working tree is dirty. A fast-forward checkout aborts if it would
overwrite local modifications or untracked files, so every dirty path was
classified against the merge's file set before planning.

### Colliding — must be resolved first

| Path | Local change | Branch change | Resolution | Rationale |
| --- | --- | --- | --- | --- |
| `crates/hya-tool/Cargo.toml` | `+sha2 = { workspace = true }` | identical line, same position | **Discard local** | Verified byte-identical to the branch blob. Discarding loses nothing; the merge restores the same content. |
| `crates/xtask/src/startup_bench.rs` | 11+/12− | 25+/24− | **Discard local** | The local edit is pure `cargo fmt` reflow — line wrapping only, zero semantic change (verified by reading the full diff). The branch's version supersedes it, and `cargo fmt --all --check` in the gate proves the result is correctly formatted. |
| `.trellis/tasks/07-30-modular-harness-native-swarm-runtime-refresh/` (untracked, 7 files + `research/`) | untracked local copies | branch tracks 13 files at this path | **Back up, remove, reconcile after merge** | Local copies **differ** from the branch's tracked versions in all 6 compared files. Deleting them outright would lose whatever a later session wrote. |

### Non-colliding — survive the fast-forward untouched

The branch touches none of these, so they stay dirty across the merge and remain
the property of their own in-flight tasks:

- `crates/hya-sdk/src/{reducer,store,types}.rs` (modified)
- `.trellis/tasks/07-23-remove-rust-tui/{prd.md,task.json}` (modified)
- `fixtures/{agents.json,display_golden.json,live_session_turn.jsonl,live_tool_turn.jsonl,turn_stream.jsonl}` (deleted)
- `imgs/Hya icon v7.png` (deleted)
- untracked: `.trellis/tasks/07-21-grok-build-provider/`,
  `.trellis/tasks/07-22-review-and-merge-open-prs/`,
  `.trellis/tasks/07-23-repository-root-cleanup/`,
  `docs/assets/8bit-examples/`, `docs/assets/hya-icon.png`, `docs/research/`,
  `tests/fixtures/`
- untracked, created by this task tree's own planning and therefore also
  non-colliding (the branch predates them): `.trellis/tasks/08-05-e2e-suite-hardening/`,
  `08-05-land-swarm-branch-to-main/`, `08-05-ci-gate-e2e-tracks/`,
  `08-05-e2e-swarm-tool-scenarios/`, `08-05-coverage-baseline-llvm-cov/`,
  `08-05-matrix-toml-runner/`

Three pre-existing stashes (`pre-sync-main-leftover-plan`,
`pre-sync-main-2026-07-01`, `pre-merge-agents-local-overview`) are untouched by
a fast-forward and are recorded in the backup for completeness.

The `fixtures/` deletions plus untracked `tests/fixtures/` are a file move
belonging to `07-23-repository-root-cleanup`. Leaving them dirty is deliberate:
this task must not absorb another task's deliverable.

## Verification strategy

The branch's own CI never ran this exact tree on `main`'s ref, so the gate runs
locally **before** the push, in the order the workflow uses:

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo build --workspace`
4. `cargo test --workspace --jobs 1 --exclude hya-e2e`
5. `cargo build -p hya-backend --bin hya-backend`
6. `cargo test -p hya-e2e -- --test-threads=1` — expect 19/19
7. `bash scripts/verify-no-http.sh`
8. Track T: `cargo build --locked -p hya -p hya-backend -p hya-ts --bins`, then
   `bun install --frozen-lockfile && bun run typecheck && bun run build && bun test`
   in `packages/hya-tui-ts`

Step 4 deliberately uses `--exclude hya-e2e` and step 6 uses
`--test-threads=1`. This is the configuration `docs/testing/process-e2e.md`
requires, and it is what child 2 will encode into CI. Running it here first also
de-risks that child.

Note the gate is run against the **merged tree with the non-colliding dirty
paths still present**. If any step fails, determine whether the dirty paths
caused it before blaming the merge.

## CI state finding (discovered during execution)

The plan assumed the merged tree needed to be gate-green before publishing.
Checking the actual GitHub Actions history changed the framing:

| Tree | CI result | Failing step | Failing files |
| --- | --- | --- | --- |
| `main` @ `156d0ad3` (current tip) | **failure** | `fmt` | `crates/xtask/src/startup_bench.rs` (5 hunks) |
| branch @ `3c07de55` | **failure** | `fmt` | 7 files under `crates/hya-e2e` (11 hunks) |

`main`'s CI has failed on every one of its last 5 commits, all at `fmt`. So the
merge does **not** turn a green `main` red — it swaps one `fmt` failure for
another. The branch's version of `startup_bench.rs` is itself fmt-clean, i.e.
the branch already fixes `main`'s current failure.

A local-toolchain-drift explanation was considered and **refuted**: local
`rustfmt 1.8.0-stable` (rustc 1.91.1, 2025-11-07) flags exactly the same seven
files that CI's `dtolnay/rust-toolchain@stable` flags, so `cargo fmt --all` run
locally produces the fix CI wants. There is no `rust-toolchain` pin file in the
repo, so this agreement is a fact about today, not a guarantee.

Consequence for step 7: `cargo fmt --all --check` **will fail** on the merged
tree. That is a known, pre-existing, one-command condition — not a signal that
the merge went wrong. Whether to fix it inside this task (it is formatting-only,
zero semantic risk) or defer it per the PRD's "pre-existing failures get their
own task" is a scope decision for the user, taken at the step-4 gate.

Consequence for child 2 (`08-05-ci-gate-e2e-tracks`): that task cannot claim a
working gate while `fmt` fails first and aborts the job — every step after
`fmt` on both branches is currently **unverified in CI**, including the
workspace test run.

## Push and reversibility

`main` and `origin/main` are in sync at `156d0ad3`. Pushing publishes 75
commits / 517 files / +91,107 lines to `github.com/anyin233/hya`.

- Before push: `git reset --hard 156d0ad3` restores `main` completely.
- After push: recovery needs a revert commit or a force-push, which is the
  user's call, not this task's.

The push is therefore the point of no easy return and gets an explicit
confirmation checkpoint in `implement.md`, separate from the merge itself.

## Worktree disposition

`.worktrees/modular-harness-native-swarm-runtime-refresh` holds branch
`codex/modular-harness-native-swarm-runtime-refresh`, which becomes identical to
`main` after the merge. It also holds a 337 MB `target/debug/hya-backend` and a
warm build cache.

Recommendation: **keep it until the sibling children are done**, then remove.
Its warm `target/` is genuinely useful for children 2–5, and removing it early
just forces a full rebuild. Record the decision either way; do not leave it
undecided.

## Risks

| Risk | Mitigation |
| --- | --- |
| Gate fails on merged `main` (branch CI never gated Track P) | Run the full gate before push; a failure blocks the push and gets its own task rather than a hurried fix here |
| Reconciled `07-30` task artifacts silently lose the local edits | Back up to the scratchpad first, diff after merge, present the differences rather than auto-choosing |
| Another session pushes to `main` mid-task | `--ff-only` fails loudly; re-plan instead of forcing |
| Vendored `sevenz-rust2` trips clippy/fmt in the workspace gate | It is already in the branch's tree and the branch's own CI ran fmt/clippy on it; if it fails, that is a pre-existing branch defect, recorded not patched here |
