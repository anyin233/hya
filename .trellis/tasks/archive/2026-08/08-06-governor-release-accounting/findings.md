# Findings — governor release accounting

Three observations, three decisions, per PRD R1. Written by the orchestrator from
the investigation's results (the investigating agent's harness blocked its own
report file); the durable decisions live in
`.trellis/spec/backend/task-tool.md`, which is the actual acceptance criterion.

## Observation 1 — release window: **REAL DEFECT, user-visible**

The PRD recorded the impact as "not demonstrated — nobody has produced a spawn
that was wrongly rejected". It is now demonstrated.

The experiment R2 named was built: at `per_run_budget = 1`, admit a single-member
background batch, poll the store until the member's row reads
`Completed + logical_released`, then immediately attempt a second spawn on the
same root.

| Run | Window open at journal-finalize | 2nd spawn `Overloaded` | Admitted |
| ---: | ---: | ---: | ---: |
| 1 | 49/50 | 49 | 1 |
| 2 | 48/50 | 48 | 2 |
| 3 | 47/50 | 47 | 3 |
| 4 | 47/50 | 47 | 3 |
| **Total** | **191/200 (95.5 %)** | **191** | 9 |

The decisive detail is not the 95.5 % rate but the **exact correlation**: in every
run the `Overloaded` count equalled the open-window count. The rejection tracks
the governor still holding the debit, so this is a mechanism, not timing noise.

**Not fixed here, per the PRD constraint.** A regression test
`released_capacity_is_visible_to_a_concurrent_spawn_on_the_same_root` is left
`#[ignore]`d (20 iterations), asserting the *intended* invariant so it fails
today — verified failing on run 0 with `Err(Overloaded)`.

`#[ignore]` was chosen over inverting the assertion deliberately: a test asserting
the buggy behaviour would read as if the behaviour were intended, and the eventual
fix would have to rewrite it rather than simply un-ignore it.

**Carried into the follow-up task:** the fix is a design question. Per-member
release at finalize time is the wrong answer — the debit is `cardinality` units
released as one unit by the owner, so early per-member release returns capacity
for members still running.

## Observation 2 — exactness: **ACCEPTED DESIGN**, and the PRD was too pessimistic

The PRD held that `remaining_budget` cannot prove exactness because
`release_operation` clamps with `.min(per_run_budget)`, so the test name
overpromises.

Verified in `crates/hya-core/src/orchestrator.rs:242-253`:
`budgets.operations.remove(&operation)` happens **before any arithmetic** and the
function returns `false` when the debit is absent. Therefore:

1. A repeated release is a genuine **no-op**, not a silent double-credit. The
   `.min(per_run_budget)` clamp is belt-and-braces, not the thing preventing
   corruption.
2. The **boolean return is a precise observable**: `true` exactly once, `false`
   thereafter.

So exactness *is* provable. R3's first branch was taken — give the test a real
observable and keep the name, which now describes what it checks. No new accessor
was needed (`release_operation` is already public and non-destructive when
absent). Semantics and clamp untouched.

### A second weakness the PRD did not record

`remaining_budget` falls back to `per_run_budget` when the root has **no** entry.
At `per_run_budget = 1`, a reading of `1` is therefore also what a never-debited
or wholly-dropped root reports — so the old bounded poll could have passed with
**no release happening at all**.

### Mutation gate — the receipt-oracle trap, closed

Each mutation applied, run, then reverted (`git diff` clean after each):

| Mutation | Result |
| --- | --- |
| M1 — `get` instead of `remove` (a real double-credit) | **FAILED as intended** |
| M2 — owner releases twice | **PASSED** — confirms double release is genuinely a no-op |
| M3 — owner never releases | **FAILED as intended** (poll `Elapsed`) |

The decisive comparison, run explicitly: **under M1 the old test passed and the
new test failed.** That is exactly the failure mode the previous round shipped
with the receipt oracle, and it is now closed.

### A correction to this task's own implement.md

`implement.md` step 1 asked for the mutation "make the release fire twice and
confirm the new assertion fails". That mutation (M2) **does not fail, and should
not** — M2 is the proof that a double release is harmless. No assertion can or
should detect it.

The corruption worth catching is crediting the budget *without* retiring the
debit, which is M1. The instruction was wrong; the investigation followed the
evidence instead of the instruction, which is the correct outcome.

## Observation 3 — drain loop: **ACCEPTED DESIGN**, invariant recorded

Re-verified against current code rather than against the task text:
`SessionEngine.spawner` is a plain owned field (not `Weak`), written only by
`with_spawn_sender`, and every `self.spawner` use is a read. Production hands the
supervisor an `engine.clone()` held for its whole life, so `rx.recv()` cannot
return `None` in production; the drain branch is reachable only from the test
helper `spawn_team_supervisor`. The ownership argument holds.

R4's second branch was taken, for the PRD's own reason: severity is **entirely
contingent** on that ownership, so the durable risk is ownership changing
unnoticed. A code change would make the dead branch safe; a recorded invariant
makes the *next* change safe.

Recorded in three places, none of them a task archive (the PRD rules that out):

1. the `with_spawn_sender` **ownership site** — where the invariant would be
   broken;
2. the drain branch itself, pointing back to it;
3. `.trellis/spec/backend/task-tool.md`.

All three name `fail_after_claim`'s `std::future::pending::<()>()`
(`runtime.rs:2842`) as why it would matter. The supervisor was **not** made
stop-aware — dead branch, and the PRD forbids that scope creep.

## Verification

Per-step exit codes, not a wrapper status: `fmt` 0 · `clippy -D warnings` 0 ·
`matrix-check` 0 (36 scenarios) · workspace **238 binaries, 1310 passed, 0
failed, 4 ignored** · `hya-backend` build 0 · e2e **18 binaries, 30 passed, 0
failed** · `verify-no-http.sh` 0.

Ignored moved 3 → 4, which is exactly the new `#[ignore]`d repro. Passed, failed
and binary counts match the baseline, so no test was lost.

No governor, admission-journal, or spawn-path behaviour was changed by this task.
