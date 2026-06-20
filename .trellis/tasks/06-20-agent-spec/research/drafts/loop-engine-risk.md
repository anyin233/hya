# Loop Engine Risk Review: Two-Agent Gate

Lens: skeptical stress test of the proposed `loop` mode from `prd.md` D5 and
Assumptions A1-A5. This extends the already-reviewed goal/team design without
starting implementation.

## Executive Position

Loop mode is viable only if yaca treats the two-agent gate as a constrained state
machine, not as a conversation between two models. The verifier is the only
success judge. The planner is only a next-directive generator. The engine owns
budget, caps, cancellation, persistence, and stop authority. If any model can
override those boundaries, loop mode becomes a costly self-ratifying auto-agent.

The central design risk is the planner context dilemma: the planner must know
enough about the worker iteration to plan well, while the token-efficiency
invariant says it must not absorb full worker or child-session transcripts. The
answer should be a bounded Loop Evidence Envelope that extends the existing Team
Evidence Envelope pattern. If the envelope is too thin, the planner repeats vague
directives. If it is too rich, loop mode erases the whole token-efficiency reason
for team mode.

## Recommended Gate Contract

Each iteration has an engine-owned sequence:

1. `Worker`: run one worker session from the current directive. The worker may
   spawn a team, but the iteration cannot advance to the gate until the worker is
   idle and all iteration-owned teams are terminal, or a per-iteration cap has
   produced a partial/capped outcome.
2. `Evidence`: project a bounded Loop Evidence Envelope into the loop record.
3. `Verifier`: cheap, tool-less, transcript/evidence-only. Emits a structured
   grade, gaps, and whether the target is satisfactorily met.
4. `StopDecision`: engine-owned. Budget/caps/cancellation always win. If A1 is
   enabled, verifier satisfaction can stop the loop. The planner cannot stop or
   approve success.
5. `Planner`: strong, tool-less, called only when the engine decided to continue.
   Emits exactly one next directive plus rationale tied to verifier gap ids.

Authority table:

| Question | Authority | Rule |
|---|---|---|
| Did the loop hit budget, wall-clock, token, cancellation, or no-progress caps? | Engine | Stop immediately; no model override. |
| Is the open-ended target satisfactory enough for early stop? | Verifier + engine threshold | Stop only if `satisfied=true`, `score >= threshold`, `critical_gaps=[]`, and evidence quality is not claim-only. |
| What should the next worker do? | Planner | Only if the engine continues; directive must target verifier gap ids or a stated strategy shift. |
| Can the planner declare success or ignore the verifier? | Nobody | Planner success claims are advisory text and must not affect stop. |
| Can the verifier prescribe a detailed plan? | No | Verifier reports evidence, score, and gaps; planner owns directive synthesis. |

This avoids planner/verifier disagreement turning into ambiguous authority. If the
planner claims progress but the verifier denies it, the loop continues only if
budget remains, and the planner must address the verifier's concrete gaps. If the
verifier says satisfied but the planner would have concerns, the planner is not
called in default mode because paying for a strong model after a terminal verdict
creates cost and authority confusion. Debug builds may optionally record a cheap
audit planner, but that is not v0 behavior.

## Central Context Contract: Loop Evidence Envelope

The planner should see enough to plan, but never the full worker transcript. Use a
Loop Evidence Envelope persisted per iteration and rendered into both verifier and
planner prompts.

The latest iteration envelope should include:

- `loop_id`, `iteration`, `worker_session_id`, `worker_directive`, and terminal
  outcome: `Completed | Capped | Cancelled | Failed`.
- Worker final summary, capped to a small bullet budget, clearly marked as a
  claim unless backed by tool output or team evidence.
- Nested Team Evidence Envelope when a team was used: team ids, member ids, task
  statuses, member result summaries, commands/checks run with exit codes, changed
  files, unresolved blockers, tool errors, child session ids, and aggregate token
  usage. Never include child transcripts or hidden reasoning.
- Direct evidence summary: commands run, exit codes, truncated outputs, files
  changed, artifact paths, tests/checks attempted, failures, and blockers.
- TokenLedger slice for this iteration: worker, verifier, planner, lead, child
  teams, model/provider, actual-vs-estimated confidence, and remaining budget.
- Progress fingerprint: stable hash of critical gaps, touched files, final check
  outputs, and planner directive family. This is used for oscillation detection.

Planner context should be:

- The loop target description and non-negotiable constraints.
- Iteration index, remaining iterations, and remaining token/time budget.
- Last two iteration envelopes in full bounded form.
- All older iterations as a rolling summary: directive, score, critical gaps,
  outcome, changed-files summary, progress fingerprint, and stop/cap events.
- The last verifier verdict in full.

Planner context should not include:

- Full worker transcripts.
- Full child-session transcripts.
- Hidden reasoning.
- Unbounded diffs or command outputs.
- Direct database/tool access. If the planner needs more information, its next
  directive should tell the worker to inspect or summarize it in the next
  iteration.

Concrete rule: if evidence is missing from the envelope, the verifier must grade
down for insufficient evidence, and the planner's next directive should first
collect the missing evidence. Do not fix missing context by giving the planner
tools or raw transcripts.

## Ranked Risks And Mitigations

### 1. Ambiguous Planner/Verifier Authority

Failure mode: the verifier says the target is not met, but the planner says the
work is good enough and emits a cleanup directive or a stop request. Or the
verifier says satisfied based on weak claims while the planner would have found a
serious gap. This creates premature stop, pointless iterations, or operator
confusion.

Mitigation:

- Encode authority in types: `LoopVerifierVerdict` has `satisfied`; planner output
  does not.
- Engine stop decision runs between verifier and planner. If stopped, planner is
  skipped.
- Planner output schema should not contain `done`, `satisfied`, or `stop` fields.
- If a provider returns such text anyway, store it as rationale only and ignore it
  for control flow.
- Emit explicit `LoopGateDecision` events so the UI can show `continued because
  verifier score 72 < 90`, not just model prose.

### 2. Planner Context Too Thin Or Too Expensive

Failure mode: with only verifier gaps, the planner gives vague repeats like
"improve the implementation". With full transcripts, the strong planner consumes
the same context the architecture was designed to avoid.

Mitigation:

- Make Loop Evidence Envelope a first-class contract, not a prompt convention.
- Use full bounded envelope for the last two iterations, rolling summary for
  older iterations.
- Include objective evidence and failure causes, not just worker narrative.
- Include child session ids as references for human/debug traceability, not as
  planner-readable context.
- Add tests that assert planner prompts contain envelope fields but not child
  assistant text by message provenance.

### 3. Early Stop Versus Fixed Budget Confusion

Failure mode: users think N means exactly N iterations, but A1 stops early. Or
users expect early stop, but the verifier score threshold is too conservative and
burns all N iterations.

Mitigation:

- Define N as a maximum budget by default. Exact-N refinement is an explicit
  `stop_when_satisfied=false` mode.
- Default early stop requires all of: `satisfied=true`, `score >= 90`, no critical
  gaps, and evidence quality `supported` or `verified`.
- For targets containing explicit checks, the verifier should require check output
  in the envelope. Claim-only summaries cannot early-stop.
- Surface stop policy in `loop_status`: `budget=7/10`, `early_stop=true`,
  `threshold=90`, `last_score=86`, `critical_gaps=2`.

### 4. Oscillation And No-Progress Loops

Failure mode: the planner keeps emitting semantically identical directives; the
worker keeps changing the same files; the verifier keeps returning the same gaps.
The hard N cap eventually stops it, but only after wasting expensive iterations.

Mitigation:

- Store `directive_fingerprint`, `gap_fingerprint`, and `evidence_fingerprint` per
  iteration.
- Default `max_no_progress_iterations=3`: stop with `LoopOutcome::NoProgress` when
  score delta is below 5 points and critical gap fingerprints are unchanged for 3
  consecutive iterations.
- Reject a planner directive whose fingerprint matches one of the last two
  directives unless it includes `strategy_change=true` and names what changed.
- Verifier verdict should include `regressions[]` separately from `gaps[]` so the
  planner can distinguish stuck work from backsliding.

### 5. Cost Blowup From Strong Planner Plus Worker/Team Per Iteration

Failure mode: each loop iteration can include a strong planner call, a long worker
session, and possibly a full team. `N * (worker + team + verifier + planner)` can
exceed the user's budget while still looking bounded by iteration count.

Mitigation:

- TokenLedger must tag entries with `loop_id`, `iteration`, and `role`:
  `worker | verifier | planner | team_member | lead | tool_estimate`.
- Preflight computes worst-case planned spend from N, per-iteration caps, team
  caps, verifier max tokens, and planner max tokens. Show this before starting
  non-interactive loop mode.
- Skip planner call on terminal verifier success.
- Default planner prompt budget should be bounded independently of worker context:
  max input from loop state, max output around 2k tokens.
- Add aggregate caps: per-loop token cap, per-iteration token cap, per-team cap,
  and planner-call cap. Aggregate cap hit stops without planner.
- TUI/CLI should show per-iteration spend and projected cost at current burn rate.

### 6. Team Mode Composition And Nested Gates

Failure mode: a loop worker spawns a team whose members keep running while the
gate starts, or a worker starts a nested goal/loop. Now there are multiple
autonomous controllers in one session tree, with unclear cancellation and budget
ownership.

Mitigation:

- V0 rule: one autonomous driver per session tree. A loop worker may use team
  mode, but child sessions cannot start their own goal or loop unless explicitly
  allowed in a later design.
- Iteration-owned teams must be terminal before gate evaluation, or the worker
  iteration is marked capped/failed with active-team evidence.
- Root loop `CancellationToken` is parent of worker, team members, verifier, and
  planner. Cancel loop means cancel all descendants.
- Team caps are inherited from design.md, but loop adds aggregate caps across all
  teams in the loop.
- Evidence envelope must include active/failed/cancelled team state. Hidden active
  teams are a correctness bug, not a UI issue.

### 7. Resume Mid-Loop And Gate Idempotency

Failure mode: process crashes after the worker finished but before verifier; or
after verifier but before planner; or after planner produced a directive but
before the next worker. On resume, yaca could duplicate model calls, skip a gate,
or generate a different directive.

Mitigation:

- Persist `LoopRun`, `LoopIteration`, and `gate_phase` as event-sourced state:
  `WorkerRunning | EvidenceBuilt | Verifying | Verified | Planning | Planned |
  IterationComplete | Stopped`.
- Use idempotency keys for gate calls: `(loop_id, iteration, role)`.
- If resume sees `EvidenceBuilt`, run verifier once. If it sees `Verified`, run
  planner if continuing. If it sees `Planned`, reuse persisted directive rather
  than regenerating.
- Resume baselines reset for wall-clock/token display like goal mode, but total
  TokenLedger entries remain attached to the loop run.

### 8. Cheap Verifier Drift And Weak Evidence Standards

Failure mode: the cheap verifier grades natural-language claims as success, or
oscillates grades due to prompt sensitivity. Open-ended targets are more ambiguous
than goal conditions, so the risk is higher than goal mode.

Mitigation:

- Verifier schema includes evidence quality, not just score:

```json
{
  "score": 0,
  "satisfied": false,
  "confidence": "low|medium|high",
  "evidence_quality": "missing|claim_only|supported|verified",
  "critical_gaps": [{"id":"G1","description":"...","needed_evidence":"..."}],
  "regressions": [],
  "progress_fingerprint": "...",
  "reason": "..."
}
```

- Prompt rule: claims are never enough for `verified`; command outputs, changed
  file summaries, or structured team evidence are required.
- Malformed verifier JSON should be treated as `satisfied=false`, score 0, and
  count against gate failure/no-progress caps. It should not create an infinite
  retry loop.

## Recommended Defaults For A1-A5

- A1 early stop: keep default `true`, but define it as verifier-threshold stop,
  not planner agreement. Defaults: `threshold=90`, `critical_gaps=[]`, evidence
  quality at least `supported`; if explicit checks exist, require `verified`.
  Users who want all N refinement passes set `stop_when_satisfied=false`.
- A2 gate independence: keep both gate agents tool-less. The verifier sees target
  plus current iteration transcript/evidence. The planner sees target, verifier
  verdict, bounded Loop Evidence Envelope, rolling history, and budgets. Neither
  sees raw child transcripts or can call tools.
- A3 iteration session model: keep one fresh worker session per iteration. The
  planner does not maintain an ever-growing chat; it receives reconstructed
  compact LoopState each time. This keeps resume deterministic and avoids hidden
  context accumulation.
- A4 model tiers: verifier uses the cheap goal evaluator tier, temp 0, strict JSON,
  small max output. Planner defaults to the strongest configured planning tier
  (`ultrabrain`/strong), temp 0 or low, strict JSON, bounded input, bounded output.
  Allow lowering planner tier per config because cost is multiplicative by N.
- A5 caps: require explicit `N` on loop creation, enforce `1 <= N <= 100` by
  default, inherit goal per-turn/time/token caps, and add loop-specific aggregate
  caps plus `max_no_progress_iterations=3`. Aggregate cap hit stops immediately;
  per-iteration worker cap can still run verifier/planner on partial evidence if
  aggregate budget remains.

## Deterministic Test Plan With FakeProvider

The loop gate should be testable without network. Add `LoopVerifier` and
`LoopPlanner` traits backed by FakeProvider scripts in production-shaped tests.

Core deterministic tests:

- `loop_false_false_true_stops`: worker scripts produce three envelopes; verifier
  scripts scores 40, 76, 93; planner scripts two directives. Assert exactly three
  workers, three verifiers, two planners, terminal `Satisfied`.
- `planner_done_ignored_when_verifier_false`: planner rationale says "done" but
  verifier `satisfied=false`; engine continues until budget/cap.
- `planner_skipped_on_terminal_success`: verifier satisfies on iteration 1; assert
  no planner provider call is made.
- `budget_exhaustion_wins`: verifier false for N iterations; assert stop reason
  `BudgetExhausted` and no extra worker/planner call.
- `exact_budget_mode`: `stop_when_satisfied=false`; verifier satisfied early, but
  planner still receives feedback and loop runs exactly N unless other caps fire.
- `no_progress_detection`: three consecutive verdicts share gap fingerprint and
  score delta < 5; assert `LoopOutcome::NoProgress` before N.
- `repeated_directive_rejected`: planner emits same directive hash twice without
  `strategy_change`; assert typed gate error or forced replan, not worker rerun.
- `malformed_verifier_json_counts`: malformed verifier output becomes unmet score
  0, emits diagnostic event, and counts toward cap.
- `malformed_planner_json_stops_gate`: planner malformed output cannot become a
  worker directive; stop or surface `GateFailed` rather than guessing.
- `team_envelope_visible_no_child_transcript`: worker uses two child sessions;
  planner/verifier prompt includes Team Evidence Envelope but not member assistant
  text by message-id provenance.
- `resume_each_gate_phase`: crash/replay from `EvidenceBuilt`, `Verified`, and
  `Planned`; assert idempotent provider calls and reused persisted directive.
- `token_ledger_complete`: each iteration records worker, verifier, planner, and
  team usage tagged by loop id and iteration, including estimated local usage.
- `cancel_fans_out`: cancel root loop while worker team, verifier, or planner is
  active; all descendants observe cancellation and no active team remains.

## Roadmap And Validation Gate Changes

The current `implement.md` has goal in Phase 6 and Team Evidence Envelope in
Phase 7. Loop mode should not be bolted on at the end. It should be inserted where
the shared autonomous iteration driver is being proven.

Recommended roadmap deltas:

- Update Phase 6 from "Goal engine" to "Autonomy driver + goal gate". Build the
  shared iteration driver, cap accounting, transcript/evidence window builder,
  gate trait shape, and goal's single-verifier gate as the first implementation.
- Add Phase 6.5 or split Phase 6 into `6a goal` and `6b loop single-worker gate`.
  Deliver loop state, `loop_set/status/clear`, `LoopVerifier`, `LoopPlanner`,
  FakeProvider scripted gates, TokenLedger loop tags, early stop, budget exhaust,
  no-progress detection, and resume for non-team workers.
- Keep current Phase 7 as the goal/team hazard gate, but extend acceptance to
  prove both goal and loop consume the Team Evidence Envelope without seeing child
  transcripts.
- Extend Phase 10 resume hardening to include mid-loop gate phases and idempotency
  keys.
- Extend Phase 12 TUI with a LoopBar beside GoalBar: target, iteration N, score,
  gaps, next directive, spend, projected burn, stop policy.
- Extend Phase 13 manual QA with one loop that stops early and one that exhausts N
  by setting `stop_when_satisfied=false`.

Validation gates that must be explicit before implementation starts:

- Type/schema gate: verifier and planner JSON schemas are fixed in design.md.
- Evidence gate: prompts for verifier/planner include envelope fields and exclude
  child transcripts, tested by provenance.
- Cost gate: TokenLedger reports projected max and actual per iteration.
- Authority gate: tests prove verifier/engine stop authority and planner-only
  directive authority.
- Resume gate: replay from every gate phase is deterministic.
- Composition gate: loop plus team cannot leave active child sessions at gate or
  after cancellation.

## Manifest Suggestions Before Sub-Agent Start

If sub-agent mode is used after spec review, add this file to both manifests:

```jsonl
{"file":".trellis/tasks/06-20-agent-spec/research/drafts/loop-engine-risk.md","reason":"Skeptical loop-mode risk review covering two-agent gate authority, Evidence Envelope context bounds, budgets, FakeProvider tests, team composition, resume, and roadmap deltas."}
```

Also ensure `implement.jsonl` includes `prd.md`, `design.md`, `implement.md`, and
`research/goal-driven-verification.md`; `check.jsonl` should include this risk
draft plus the same source artifacts so reviewers can verify the loop additions
against the original goal/team constraints.
