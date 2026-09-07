# Loop Engine — Unify Lens (architect draft, distilled)

Companion to `loop-engine-risk.md`. The merged result lives in design.md §9–§10.

## Core decision: one unified driver, two swaps

`IterationDriver<G: IterationGate, X: IterationExecutor>` owns turn driving, caps,
cancellation, token ledger, projection, event emission, persistence — and contains
**zero `if goal/loop` branches**. Goal and loop diverge in EXACTLY two seams:

| Seam | Goal | Loop |
|---|---|---|
| `IterationExecutor` | `LeadTurnExecutor` — reuse the lead `SessionId`, run one turn | `WorkerSessionExecutor` — spawn a FRESH child session per iteration, run to completion bounded by per-iteration caps |
| `IterationGate` | `GoalGate` — 1 cheap verifier, transcript-only → met? | `LoopGate` — cheap verifier + strong planner, both tool-less |

Driver loop: check caps/cancel → `executor.run_iteration(directive, continuity_brief)`
→ ledger.record → `gate.judge(outcome)` → `GateOutcome::{Stop{verdict,reason} |
Continue{verdict, next_directive}}`. The **gate** decides success; the **driver**
decides caps. `GoalEvaluator` (existing §9) becomes the inner type `GoalGate` wraps
— minimal churn.

## Two-agent loop gate (control flow)

1. **Verifier** (cheap tier) runs first; sees ONLY `(target, this iteration's
   TranscriptProjection)`. Emits `{score 0..100, satisfied, evidence_quality, gaps,
   regressions, iteration_summary ≤500c, progress_fingerprint, reason}`.
2. **Early-stop short-circuit** BEFORE the planner: if `stop_when_satisfied &&
   satisfied && score≥threshold && critical_gaps==[] && evidence_quality≥supported`
   → `Stop{Satisfied}`. Saves the expensive planner call.
3. **Planner** (strong tier) runs only when continuing; sees `(target, bounded
   history of iteration_summaries, last verifier verdict, planner_notes)` — NEVER
   raw worker/child transcripts. Emits `{directive, continuity_brief, planner_notes}`.
   Planner output has **no stop/done/satisfied field** (authority is the engine's).

## Continuity discipline (no surface grows linearly in N)

- Worker session context = system + directive + continuity_brief + that iteration's
  turns → O(1) in N.
- Planner context = target + sliding window of iteration_summaries + last verdict +
  bounded planner_notes (planner rewrites each iter) → O(window).
- Verifier context = target + one iteration projection → O(1).
- Sliding compaction when history exceeds the window; inject a `[compacted earlier
  iterations]` marker so the planner knows.

## Team composition (zero new mechanism)

A loop worker is a normal child session; it may call `team_*`; the existing Team
Evidence Envelope (§10) projects team results into the worker transcript; the
verifier judges the projection. Worker-spawned teams are reaped when the iteration
ends (worker cancel token trips → supervised members cancel). Lead-spawned teams are
untouched (same no-cascade rule as goal).

## A1–A5 resolution (both planners agree)

- A1: `stop_when_satisfied=true` default; numeric `satisfaction_threshold=90` +
  critical_gaps empty + evidence_quality≥supported (verified if target names a check).
  `false` ⇒ exhaust N (keep refining).
- A2: both agents tool-less; verifier transcript-only/history-blind; planner sees
  bounded structured history only.
- A3: fresh worker session per iteration; planner carries continuity; worker O(1).
- A4: planner = strong tier (`loop_planner` category); verifier = cheap tier
  (alias `goal_evaluator`); both via the §8 category→model resolver.
- A5: explicit budget N required, hard ceiling 100, per-iter caps (30 turns/500k
  tokens), loop wall-clock 7200s, total tokens inherit 2M, `max_no_progress=3`.

## design.md changes (driven by this merge)

§9 rename→Completion engine (driver + 2 modes); §10 generalize + Loop Evidence
Envelope + per-iter team reap; §3 loop events; §5 loop_* tools; §6 loop_run +
loop_iteration tables + gate_phase + token_ledger.completion_run_id; §8 add
`Creating→ForceDeleting` row; §13 add unify+authority decision; §14 add gate→
directive→executor + worker→cancel→reap seams; §15 add loop caps/authority/tiers.
