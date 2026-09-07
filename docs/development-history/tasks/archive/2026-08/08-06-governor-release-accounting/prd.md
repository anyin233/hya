# Investigate governor release accounting

Follow-up from the E2E hardening round. These are **observations surfaced while
fixing something else**, not diagnosed defects. Evidence in
`.trellis/tasks/archive/2026-08/08-05-land-swarm-branch-to-main/findings.md`.

## Goal

Decide whether the three observations below are real problems or accepted
design, and record the decision so they stop being re-discovered.

## Why this is an investigation, not a fix

All three were found while repairing a stale test. None was chased to a
conclusion, deliberately — the task that found them was scoped to landing a
branch, not to redesigning admission accounting. Each is written here with its
actual evidential strength.

### 1. Release window between journal finalize and governor release

A member task finalizes its journal row to `Completed + logical_released` at
`crates/hya-app/src/runtime.rs:1777` via `store().finalize_admission_members(…)`,
**bypassing** hya-core's governor-releasing `finalize_spawn_admission`. The
owner returns the in-memory governor debit much later, at `runtime.rs:3044`
(`release_transient_operation`), after draining `completion_rx`, quiescing
handles, and projecting the evidence envelope.

In that window a concurrent spawn on the same root can be rejected
`Overloaded` even though the capacity is logically free.

- **Strength**: the code path is read and confirmed; the user-visible impact is
  **not** demonstrated. Nobody has produced a spawn that was wrongly rejected.
- The window is currently *tolerated* by
  `admitted_background_transient_releases_its_exact_debit_on_completion`, which
  was changed to a bounded poll rather than resolving it.

### 2. `remaining_budget` cannot prove an exact debit

`SubagentGovernor::release_operation` clamps with
`.min(self.limits.per_run_budget)` (`crates/hya-core/src/orchestrator.rs:247`).
At `per_run_budget = 1`, a double release is indistinguishable from a single
one through `remaining_budget`.

So the test named `…releases_its_exact_debit_on_completion` cannot actually
prove exactness — the name overpromises. This predates the recent changes.

- **Strength**: confirmed by reading the clamp. Whether a double release can
  actually occur is **unknown**; the point is only that this observable could
  not detect it.

### 3. Supervisor drain loop is not stop-aware

The drain loop after the request-intake `else` branch
(`crates/hya-app/src/runtime.rs`, the branch guarded by `stop_child.is_cancelled()`
since `16bde844`) does not watch `stop_child`. If intake ever closed with
handlers in flight, a subsequent `shutdown()` would block until they finish
rather than aborting — and `fail_after_claim` can await
`std::future::pending::<()>()` (`runtime.rs:2842`), which is production code.

- **Strength**: unreachable in production today. `SessionEngine` owns the
  `BoundSpawnSender` and the supervisor task holds an `engine.clone()`, so
  `rx.recv()` cannot return `None` while the supervisor lives. The branch is
  reachable only from the test helper `spawn_team_supervisor`.
- Recorded as low severity precisely because of that ownership argument. If the
  ownership ever changes, the severity changes with it.

## Requirements

- R1. For each observation, reach a decision: **real defect** (fix it),
  **accepted design** (document why), or **still unknown** (say what evidence is
  missing). Do not convert an observation into a fix without first establishing
  it can actually bite.
- R2. For (1), determine whether the window is user-visible. A test that
  provokes a concurrent spawn inside it would settle the question either way. If
  it is real, the fix is a design question — per-member early release would be
  wrong for multi-member batches, so do not reach for it reflexively.
- R3. For (2), either give the test an observable that can prove exactness, or
  rename it to what it actually checks. A test whose name overstates its
  guarantee misleads every future reader.
- R4. For (3), decide whether to make the drain loop stop-aware or to record the
  ownership invariant that makes it moot — and if the latter, put the invariant
  somewhere it will be seen when the ownership changes, not only in a task
  archive.

## Constraints

- Do not "fix" (1) by releasing the governor debit at member-finalize time. The
  operation debit is `cardinality` units released as one unit by the owner;
  per-member early release is wrong for multi-member batches.
- Do not expand (3) into making the whole supervisor stop-aware. It is a
  currently-dead branch; scope creep there buys nothing.
- Any change here touches spawn admission, which the previous round showed is
  easy to break silently. Run the full workspace suite plus Track P.

## Acceptance criteria

- [ ] Each of the three has a recorded decision with its reasoning.
- [ ] Anything classified "real defect" is fixed with a regression test that
      fails before and passes after.
- [ ] Anything classified "accepted design" is documented where a future reader
      will find it — `.trellis/spec/backend/task-tool.md` is the natural home.
- [ ] Anything still unknown states what evidence would settle it.
- [ ] `cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast` and
      `cargo test -p hya-e2e -- --test-threads=1` green.

## Out of scope

- Redesigning the governor or the admission journal.
- Performance work on the spawn path.
