# Design — governor release accounting

This is an **investigation**, not a fix task. The PRD is explicit that all three
items are observations surfaced while fixing something else, none chased to a
conclusion. R1 requires each to end as **real defect**, **accepted design**, or
**still unknown** — and warns against converting an observation into a fix before
establishing it can bite.

The deliverable is therefore *decisions with evidence*, plus only the code
changes those decisions justify.

## Observation 1 — the release window

A member finalizes its journal row to `Completed + logical_released` at
`runtime.rs:1777` via `store().finalize_admission_members(…)`, bypassing
hya-core's governor-releasing `finalize_spawn_admission`. The owner returns the
in-memory governor debit much later at `runtime.rs:3044`
(`release_transient_operation`), after draining `completion_rx`, quiescing
handles, and projecting the evidence envelope.

**Evidential strength today:** the code path is read and confirmed; the
user-visible impact is **not** demonstrated. Nobody has produced a spawn that was
wrongly rejected.

### How to settle it

R2 names the experiment: provoke a concurrent spawn on the same root inside the
window and see whether it is rejected `Overloaded`. Concretely — admit a
single-member batch at `per_run_budget = 1`, let the member finalize its journal
row, and attempt a second spawn *before* the owner reaches `:3044`.

Two outcomes, both acceptable:

- **Rejected `Overloaded`** → the window is user-visible. Classify **real
  defect**, and note that the fix is a design question, not a reflex.
- **Admitted** → something else already serialises it. Classify **accepted
  design** and record what closes the gap, so the next reader does not
  re-discover it.

### Constraint that must not be violated

**Do not fix this by releasing the governor debit at member-finalize time.** The
operation debit is `cardinality` units released as one unit by the owner;
per-member early release is wrong for multi-member batches — it would return
capacity for members that are still running. The PRD calls this out explicitly,
and it is the obvious-looking wrong answer.

If observation 1 turns out real, the fix belongs in a follow-up task with its own
design, not smuggled in here.

## Observation 2 — `remaining_budget` cannot prove an exact debit

`SubagentGovernor::release_operation` (`crates/hya-core/src/orchestrator.rs:242`)
clamps with `.min(self.limits.per_run_budget)`, so at `per_run_budget = 1` a
double release is indistinguishable from a single one *through
`remaining_budget`*. The test named
`admitted_background_transient_releases_its_exact_debit_on_completion`
(`crates/hya-app/tests/spawn_admission.rs:495`) therefore does not prove what its
name claims.

### A correction to the PRD's framing, found while reading

The PRD treats exactness as unprovable through the available observables. It is
not. `release_operation` reads:

```rust
let Some(debit) = budgets.operations.remove(&operation) else {
    return false;
};
```

The debit is **removed from the map before any arithmetic**, and the function
returns `false` when it is absent. So:

1. A double release is already a **no-op**, not a silent double-credit — the
   clamp is a belt-and-braces guard, not the thing preventing corruption.
2. The **boolean return is itself a precise observable**: `true` exactly once,
   `false` thereafter.

That reframes R3. The choice is not "prove exactness or rename"; exactness is
provable. Preferred resolution: **give the test the real observable** — assert
the first release returns `true` and a second returns `false` — and keep the
name, which then describes what it checks.

*This must be verified against the code before being relied on, not taken from
this document.* If a public accessor is missing, adding a narrow test-only one is
in scope; changing `release_operation`'s semantics is not.

## Observation 3 — the supervisor drain loop is not stop-aware

The drain loop after the request-intake `else` branch (guarded by
`stop_child.is_cancelled()` since `16bde844`) does not watch `stop_child`. If
intake ever closed with handlers in flight, a subsequent `shutdown()` would block
until they finish rather than aborting — and `fail_after_claim` can await
`std::future::pending::<()>()` (`runtime.rs:2842`), which is production code.

**Strength: unreachable in production today.** `SessionEngine` owns the
`BoundSpawnSender` and the supervisor task holds an `engine.clone()`, so
`rx.recv()` cannot return `None` while the supervisor lives. The branch is
reachable only from the test helper `spawn_team_supervisor`.

### Resolution

R4 offers two options: make the drain loop stop-aware, or record the ownership
invariant somewhere it will be seen when the ownership changes.

**Prefer recording the invariant**, for a reason the PRD itself supplies: the
severity is entirely contingent on the ownership argument, so the durable risk is
that ownership changes and nobody notices the coupling. A code change makes the
dead branch safe; a recorded invariant makes the *next* change safe. The latter
addresses the actual failure mode.

"Somewhere it will be seen" explicitly rules out a task archive. It means a
comment at the ownership site plus `.trellis/spec/backend/task-tool.md`.

**Constraint:** do not expand this into making the whole supervisor stop-aware.
It is a currently-dead branch; scope creep buys nothing.

## Blast radius

Deliberately small, and mostly documentation:

| File | Change |
| --- | --- |
| `crates/hya-app/tests/spawn_admission.rs` | give the exactness test a real observable (obs 2); possibly a window test (obs 1) |
| `crates/hya-core/src/orchestrator.rs` | test-only accessor **only if** one is genuinely missing |
| `crates/hya-app/src/runtime.rs` | invariant comment at the ownership site (obs 3) |
| `.trellis/spec/backend/task-tool.md` | record decisions for obs 1 and obs 3 |

No governor redesign, no admission-journal changes, no spawn-path performance
work — all out of scope per the PRD.

## Verification

Any change here touches spawn admission, which the previous round showed is easy
to break silently. Full workspace suite **plus** Track P, per the PRD.

Current baseline on this branch: **238 binaries, 1310 passed, 0 failed, 3
ignored**; e2e **18 binaries, 30 passed**.
