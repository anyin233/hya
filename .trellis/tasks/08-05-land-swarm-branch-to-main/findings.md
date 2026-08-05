# Findings — quality gate on merged main (2026-08-05)

Recorded per `implement.md` step 7: the gate failed, so the push (step 9) did
**not** happen. `main` is merged locally at `620611cc` and unpushed.

## Gate results

| Step | Result | Time |
| --- | --- | --- |
| 1 `cargo fmt --all --check` | **pass** | 2s |
| 2 `cargo clippy --workspace --all-targets -- -D warnings` | **pass** (zero warnings) | 20s |
| 3 `cargo build --workspace` | **pass** | 30s |
| 4 `cargo test --workspace --jobs 1 --exclude hya-e2e` | **FAIL** | 535s |
| 5–7 (backend build, Track P, verify-no-http) | not reached | — |

Step 1 passing is the payoff from the `620611cc` formatting commit; `fmt` was
the step that had been failing in CI on both `main` and the branch.

## Full test picture

The first run stopped at the first failing binary. Re-run with
`--no-fail-fast` for the complete count:

```
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
→ 756 passed, 6 failed
→ 89 test binaries ok, 3 test binaries FAILED
```

| # | Test | File | On pre-merge main? |
| --- | --- | --- | --- |
| 1 | `nested_spawn_reaches_root_tree` | `crates/hya-app/tests/nested_spawn_tree.rs` | **existed and passed** |
| 2 | `nested_spawn_registers_two_generations_in_root_roster` | same | **existed and passed** |
| 3 | `tree_endpoint_attaches_roster_to_child_and_grandchild` | same | **existed and passed** |
| 4 | `queued_spawn_uses_parent_turn_binding_after_catalog_publication` | `crates/hya-app/tests/spawn_admission.rs` | branch-new file |
| 5 | `admitted_background_transient_releases_its_exact_debit_on_completion` | same | branch-new file |
| 6 | `private_info_is_opaque_and_install_does_not_mutate_registry` | `crates/hya-backend/tests/bundle_cli.rs` | branch-new file |

## Failures 1–3 are a genuine regression, not a pre-existing failure

Proven by running the same test file on a throwaway clone of pre-merge
`main` @ `156d0ad3`:

```
$ cargo test -p hya-app --test nested_spawn_tree     # pristine main 156d0ad3
test nested_spawn_registers_two_generations_in_root_roster ... ok
test tree_endpoint_attaches_roster_to_child_and_grandchild ... ok
test nested_spawn_reaches_root_tree ... ok
test result: ok. 3 passed; 0 failed
```

versus, on the branch worktree (independent of the merge — same failure, so
neither the merge nor this machine's environment causes it):

```
thread 'tree_endpoint_attaches_roster_to_child_and_grandchild' panicked at
crates/hya-app/tests/nested_spawn_tree.rs:161:6: spawn failed: Unavailable
test result: FAILED. 0 passed; 3 failed
```

The branch migrated this test's fixture from `SpawnerPlane::new()` to
`BoundSpawnSender::with_capacity(1)` alongside the durable spawn-admission work
(`53a76ec1 feat: add durable spawn admission operations`, and later
`b8c21dee feat: reject overloaded subagent spawns before creation`).

**Root cause is NOT established.** The error is `SpawnError::Unavailable`, not
`SpawnError::Overloaded`, so the new overload-rejection path is *not* the
obvious culprit and the "capacity 1 is too small" hypothesis does not fit the
observed error. `runtime.rs` has ~15 distinct `Unavailable` return sites.
Diagnosing which one fires belongs to the follow-up task, not here.

## Why this was never caught

CI on both `main` and the branch fails at the `fmt` step, which runs *before*
the test step and aborts the job. Every commit on the branch — including the
one that introduced the regression — had its test step skipped by CI. This is a
concrete instance of the risk the parent task exists to remove.

Evidence: `gh run list` shows failure on all 5 recent `main` runs and all 8
recent branch runs, every one at `STEP: fmt`.

## Cross-task implications

- **Two of the six failures are `matrix.toml`-registered scenarios**:
  `I.nested` (`nested_spawn_tree.rs`) and `I.bundle_cli` (`bundle_cli.rs`).
  The registry lists them as coverage; they are red, and nothing noticed.
  This is direct evidence for child 5 (`08-05-matrix-toml-runner`) — a registry
  no one runs is a registry that reports coverage it does not have.
- **Child 2 (`08-05-ci-gate-e2e-tracks`)** must account for this: making the
  gate real will turn `main`'s CI red on these 6 tests until they are fixed.
  Wiring the gate and fixing the failures need to be sequenced deliberately.

## Decisions taken on the fixes (2026-08-05)

Root causes for all 6 failures were established by a 7-agent diagnosis workflow
(3 investigators, 3 adversarial reviewers, 1 synthesizer); every root cause was
proven by an executed control experiment, and all three survived review. Plan:
`fix-plan.md`.

Outcome: **5 of 6 failures are stale tests, not product bugs.**

- **Fix 1** `bundle_cli.rs:155` — the test hardcoded `unsupported-in-0.34.11`
  while the product has always printed `env!("CARGO_PKG_VERSION")` (now
  `0.34.13`). Two release bumps never updated the literal. Replaced with
  `concat!(…, env!("CARGO_PKG_VERSION"))` so it cannot drift again.
- **Fix 2 + 3** `nested_spawn_tree` — two independent fixture gaps, both
  required. The shared `tests/support/mod.rs` builds its catalog with
  `BundleCatalog::from_prepared`, which deliberately yields no
  `semantic_identity_v1`, so the durable-admission path cannot compute a runtime
  fingerprint → `Unavailable`. Fixing only that advances the failure to
  `ProviderIdentityUnavailable`, because the test's bare `FakeProvider` leaves
  `configured_identity_v1` at the fail-closed default. Both are *asserted*
  product behaviours, so the tests were stale, not the product.
- **Fix 4** `admitted_background_transient_releases_its_exact_debit_on_completion`
  — the journal row and the governor debit are written by different tasks;
  commit `3024b449` made background owners reply at registration, destroying the
  implicit ordering the bare assertion relied on. Replaced with a bounded 5s
  poll. Note: the reviewers found this test can flake green (~1 in 40), so a
  single green run is not evidence the fix is unneeded.
- **Fix 5** `queued_spawn_uses_parent_turn_binding_after_catalog_publication` —
  the only product edit. `runtime.rs:3410` aborted in-flight handlers whenever
  the request loop exited, but that branch is reached both by explicit shutdown
  *and* by intake closing when the last `BoundSpawnSender` drops; commit
  `2b6269d6` added the abort for the shutdown case only, per its own design
  note. Guarded with `stop_child.is_cancelled()`.

  **This was a judgement call, not a fact**, and the user decided it on
  2026-08-05: product-side guard, over the equally-green test-side alternative
  (keep the sender alive in the test). Rationale accepted: it restores the
  semantics `2b6269d6` documented, and preserves the fixture's coverage of
  "request queued before the supervisor started".

  **Honest scope note:** this is *not* a live production bug. `SessionEngine`
  owns the `BoundSpawnSender` and the supervisor task holds an `engine.clone()`,
  so intake cannot close while the supervisor lives. The closed-intake branch is
  reachable only from the test helper. This is a latent/dead-branch correctness
  fix, and the commit message should say so rather than claim a user-visible bug
  was fixed.

## Gate result after the fixes (2026-08-05)

All seven Rust gate steps pass, plus Track T:

| Step | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo build --workspace` | pass |
| `cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast` | **1323 passed, 0 failed** (237 binaries) |
| `cargo build -p hya-backend --bin hya-backend` | pass |
| `cargo test -p hya-e2e -- --test-threads=1` | 19/19 pass |
| `bash scripts/verify-no-http.sh` | pass |
| Track T (`bun typecheck` + `build` + `test`) | **50 passed, 0 failed** (11 files, 2292 assertions) |

### Correction to an earlier number in this file

The first `--no-fail-fast` measurement reported "756 passed / 6 failed". That
capture was **incomplete** — it covered 93 test binaries, where a full run
covers 237. The six failures were real and are all fixed, but the total was
understated. The correct full-suite size is ~1323 tests.

### New flake discovered — `recovered_promotions_reconstruct_each_parent_binding`

One intermediate gate run failed with:

```
crates/hya-app/src/runtime.rs:6001: assertion `left == right` failed
  left: 0   right: 1        // resolve_recovered_admission_launches(...).len()
```

Investigated rather than waved away, because it appeared right after the Fix 5
product edit:

- The assertion is inside `resolve_recovered_admission_launches`, which has no
  call relationship to `spawn_team_supervisor_with_environment` where Fix 5
  lives.
- `git diff crates/hya-app/src/runtime.rs` confirms Fix 5 is the only change to
  that file (9 insertions, 1 deletion).
- The test passes **6/6** in isolation and the full `hya-app --lib` suite passes
  **5/5** with the guard in place.
- A second full-workspace gate run is green (1323/0).

Conclusion: load/ordering-dependent flake, observed once in two full-workspace
runs, **not** a regression from Fix 5. It is not fixed here — it is recorded so
the next person does not re-derive this. Reproduction condition: full
`cargo test --workspace --jobs 1` under load, not the lib suite alone.

## Related observations worth their own tasks

- A real (small) product window exists between a member's journal row reaching
  `Completed + logical_released` and the owner returning governor units; a
  concurrent spawn on the same root can be rejected `Overloaded` in that gap.
  Fix 4 tolerates it rather than resolving it.
- `SubagentGovernor::release_operation` clamps with
  `.min(self.limits.per_run_budget)`, so at `per_run_budget = 1` a double
  release is indistinguishable from a single one — the test named "…exact
  debit…" cannot actually prove exactness.
- Six other fixtures still build catalogs with the unverified
  `BundleCatalog::from_prepared` and pass only because they never reach
  `prepare_spawn_admission`. Any future test routing a spawn through durable
  admission from one of them hits the same wall Fix 2 just cleared.
- `bundle_cli` shares `std::process::id()` across its 7 tests; one pre-existing
  `AlreadyExists` flake was observed in 24 whole-file runs.

## Outcome

Pushed. `origin/main` moved `156d0ad3..16bde844` (80 commits, 517 files,
+91,244 / −4,886). Final local gate before the push: **1324 passed, 0 failed**
across 237 binaries, plus Track P 19/19, `verify-no-http`, and Track T 50/50.

Commits added on top of the 75 landed ones:

| Commit | Kind |
| --- | --- |
| `620611cc` `style(e2e): apply rustfmt to hya-e2e harness and scenarios` | formatting — unblocks the CI `fmt` gate |
| `75d22770` `test(bundle): derive cli version banner from CARGO_PKG_VERSION` | test |
| `8e21c89d` `test(admission): build app-test runtime from a verified catalog` | test |
| `0a574b0d` `test(admission): poll for governor debit release after completion` | test |
| `16bde844` `fix(admission): abort spawn handlers only on explicit shutdown` | product + its regression test |

`16bde844` also carries `shutdown_aborts_in_flight_foreground_handler`, added by
the check pass: the guard created a branch whose *true* side had no coverage —
`built_session_engine_shutdown_drains_supervisor` passes even with `abort_all()`
deleted entirely. The new test was falsified both ways (10/10 green with the
guard, times out with the abort removed).

Version was deliberately **not** bumped (stays `0.34.13`), decided by the user:
this task lands an existing branch and repairs its stale tests, and the single
product change is unreachable in production, so there is no user-visible
behavior change to release. `AGENTS.md`'s per-fix version rule is noted as
consciously waived here, not overlooked.

## Worktree disposition (step 10)

`.worktrees/modular-harness-native-swarm-runtime-refresh` is **kept for now**,
but it is no longer equivalent to `main`: `main` is 4 commits ahead of
`codex/modular-harness-native-swarm-runtime-refresh` (the four fix commits were
made on `main`). Its original justification — a warm `target/` for children 2–5
— has weakened, because the main checkout's `target/` is now warm too.

Recommendation for whoever picks up children 2–5: remove it
(`git worktree remove .worktrees/modular-harness-native-swarm-runtime-refresh`)
and work in the main checkout, so no one edits a stale tree by accident.

## State left behind

- `main` = `origin/main` = `16bde844`. Published.
- Rollback now requires a revert commit or a force-push — a user decision, not
  this task's.
- The unrelated pre-existing dirty paths were preserved untouched throughout and
  are still uncommitted: `crates/hya-sdk/src/{reducer,store,types}.rs`,
  `.trellis/tasks/07-23-remove-rust-tui/{prd.md,task.json}`, the `fixtures/*`
  and `imgs/*` deletions (the `07-23-repository-root-cleanup` file move). None
  were swept into any commit here.
- Backups: `/home/yanweiye/yaca-premerge-backup-2026-08-05` (dirty patch with
  `--binary`, all 86 untracked files as a tar, the `07-30` local snapshot, and
  the pre-merge tip/stash/worktree records). Safe to delete once the unrelated
  dirty work above is committed by its own task.

## CI is green — first full run in recent history

Run 31053324678 on `16bde844` (the commit carrying every code change):
**all 15 steps success**, including the two that matter most here:

| Step | Result | Note |
| --- | --- | --- |
| 9. `fmt` | success | first pass in weeks — this is what had been aborting every run |
| 13. `test` | success | the full workspace suite had **never executed** on CI in recent history |
| 15. `verify-no-http` | success | also never reached before |

So the local gate result (1324 passed / 0 failed) is now corroborated by CI on a
clean checkout with a fresh toolchain, not just on this machine.

## Post-push CI finding — a non-gating PTY test gates everything

The first CI run after the push (`d737bb28`) failed, but **not** at any Rust
step. It failed at workflow **step 8, "Test TypeScript TUI"**, on
`test/pty-smoke.test.ts:589` — `error: timed out waiting for root draft`, after
64.97s. Every step after it (`fmt`, `clippy`, `build`, `test`,
`verify-no-http`) was **skipped**.

### It is a flake, not a regression from this work

| Run | Commit | Step 8 result |
| --- | --- | --- |
| 30283950013 | `156d0ad3` (pre-merge main) | success |
| 30990755344 | `3c07de55` (branch tip) | success |
| 31053432077 | `d737bb28` | **failure** |
| 31053324678 | `16bde844` | success |
| 31053472545 | `b64118bf` | success |

`d737bb28` is `16bde844` plus a **docs-only** commit (`.trellis` markdown). The
compiled code is identical, and `16bde844` passed the same step. Locally
`bun test test/pty-smoke.test.ts` passes 3/3. Two passes and one failure on
byte-identical code is a flake on a slower/loaded runner, not a defect
introduced here.

### The structural problem this exposes — for child 2

`docs/testing/agent-matrix.md` states plainly: *"Full PTY matrix for every
feature ID is **not** required for the PR gate; PTY remains presentation smoke."*
But `ci.yml` runs a blanket `bun test` at step 8, which executes
`pty-smoke.test.ts` anyway — and because it precedes every Rust step, a flaky
test the docs call non-gating can block `fmt`, `clippy`, `build`, the whole
workspace test suite, and `verify-no-http` from running at all.

This is the *same structural defect* as the `fmt`-before-`test` ordering that
let 6 broken tests ship unnoticed: an early, non-essential step aborts the
entire job. Child 2 should fix the class, not just the instance — e.g. run the
Rust gate in a job that does not depend on PTY smoke, and make Track T's
enforced set explicitly the three registered scenarios rather than a blanket
`bun test`.

## Handover to sibling children

- **Child 2 (`ci-gate-e2e-tracks`)** — the gate command sequence is now proven
  locally end to end and is exactly what `implement.md` step 7 runs. Note the
  historical trap it must not recreate: `fmt` runs before `test` and aborts the
  job, which is precisely why 6 broken tests shipped unnoticed. Consider whether
  `fmt` should stop gating the test step at all.
- **Child 5 (`matrix-toml-runner`)** — two registered scenarios (`I.nested`,
  `I.bundle_cli`) were red while the registry advertised them as coverage. That
  is the concrete failure mode the drift check exists to catch; use it as the
  motivating example.
- **All children** — work in the main checkout, not the stale worktree.
