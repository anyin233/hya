# Close the governor release window on the same root

Follow-up from `08-06-governor-release-accounting`, which **demonstrated** this
defect rather than merely suspecting it. Evidence:
`.trellis/tasks/08-06-governor-release-accounting/findings.md` §"Observation 1".

## The defect, with its evidence

A member task finalizes its journal row to `Completed + logical_released` at
`crates/hya-app/src/runtime.rs:1777` via `store().finalize_admission_members(…)`,
bypassing hya-core's governor-releasing `finalize_spawn_admission`. The owner
returns the in-memory governor debit much later at `runtime.rs:3044`
(`release_transient_operation`).

In that window a concurrent spawn on the same root is rejected `Overloaded`
even though the capacity is logically free.

Measured at `per_run_budget = 1`, 4 runs × 50 iterations:

| Run | Window open at journal-finalize | 2nd spawn `Overloaded` | Admitted |
| ---: | ---: | ---: | ---: |
| 1 | 49/50 | 49 | 1 |
| 2 | 48/50 | 48 | 2 |
| 3 | 47/50 | 47 | 3 |
| 4 | 47/50 | 47 | 3 |
| **Total** | **191/200 (95.5 %)** | **191** | 9 |

The `Overloaded` count equalled the open-window count in **every** run. The
rejection tracks the governor still holding the debit — a mechanism, not timing
noise.

## Starting point

`crates/hya-app/tests/spawn_admission.rs` already carries
`released_capacity_is_visible_to_a_concurrent_spawn_on_the_same_root`, currently
`#[ignore]`d and running 20 iterations. It asserts the **intended** invariant, so
it fails today (verified failing on run 0 with `Err(Overloaded)`).

It was left `#[ignore]`d rather than inverted on purpose: a test asserting the
buggy behaviour would read as if that behaviour were intended. **Un-ignoring it
is the acceptance signal for this task** — no new test needs writing first.

## Requirements

- R1. A spawn on a root whose capacity is logically free must be admitted, not
  rejected `Overloaded`.
- R2. Remove the `#[ignore]` and have the test pass over its 20 iterations.
- R3. State the before/after rejection rate under the same reproducing condition
  (`per_run_budget = 1`, journal row finalized, immediate second spawn). A single
  green run proves nothing.

## Constraints — the obvious answer is wrong

- **Do not release the governor debit at member-finalize time.** The operation
  debit is `cardinality` units released as one unit by the owner; per-member
  early release returns capacity for members that are still running, which breaks
  multi-member batches. This is the reflex fix and it is incorrect.
- Do not weaken or invert the regression test to make it pass.
- Spawn admission is easy to break silently — the full workspace suite **and**
  Track P both matter (`08-05-land-swarm-branch-to-main` shipped six broken tests
  by skipping exactly this).

## Acceptance criteria

- [ ] `released_capacity_is_visible_to_a_concurrent_spawn_on_the_same_root` is no
      longer `#[ignore]`d and passes.
- [ ] Before/after rejection rates recorded with run counts.
- [ ] The debit is still released as one unit by the owner — no per-member early
      release.
- [ ] `cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast` and
      `cargo test -p hya-e2e -- --test-threads=1` green.

## Out of scope

- Redesigning the governor or the admission journal.
- The other two observations from the parent task; both were resolved there
  (exactness: accepted design with a real observable; drain loop: accepted design
  with the invariant recorded).
