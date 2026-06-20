# Research: Goal-Driven Development & Independent Verification

Source: Claude Code `/goal` docs — https://code.claude.com/docs/en/goal
(fetched during planning). This is one of the two core features.

## What `/goal` does

`/goal <condition>` sets a **session-scoped completion condition**. The agent
keeps starting new turns toward it without the user prompting each step. After
each turn, a **separate evaluator** checks whether the condition holds:
- **met** → goal auto-clears, records an "achieved" entry.
- **not met** → agent starts another turn; the evaluator's reason is fed back
  as guidance for the next turn.

Use cases: migrate a module until all call sites compile + tests pass; implement
a design doc until all acceptance criteria hold; split a large file until each
piece is under a size budget; drain a labeled issue backlog.

## How evaluation works (the key mechanism)

- Implemented as a wrapper over a **session-scoped, prompt-based Stop hook**.
- After each turn, `(condition + conversation transcript so far)` is sent to a
  **small/fast model** (defaults to Haiku). It returns **yes/no + short reason**.
- **CRITICAL CONSTRAINT**: the evaluator **does NOT call tools or read files**.
  It judges ONLY what the main agent surfaced in the transcript. So the
  condition must be phrased so the main agent's own output demonstrates it
  (e.g. it ran `cargo test` and the result is in the transcript).
- The evaluator runs on whichever provider the session is configured for.
- Eval token cost is on the small fast model — negligible vs main-turn spend.

## Writing an effective condition

A durable condition has:
1. **One measurable end state** — a test result, a build exit code, a file
   count, an empty queue.
2. **A stated check** — how the agent proves it ("`npm test` exits 0",
   "`git status` is clean").
3. **Constraints that must hold** — e.g. "no other test file is modified".

- Optional bound clause: "or stop after 20 turns" (agent reports progress vs it
  each turn; evaluator judges from the conversation).
- Max condition length: 4000 chars.

## Lifecycle / commands

- `/goal <condition>` — set (replaces any active goal); **starts a turn
  immediately** with the condition as the directive.
- `/goal` (no arg) — status: condition, elapsed, turns evaluated, token spend,
  evaluator's most recent reason.
- `/goal clear` (aliases: stop/off/reset/none/cancel) — remove active goal.
- One goal active per session at a time.
- Resume (`--resume`/`--continue`) restores an active goal; turn/timer/token
  baselines reset. Achieved/cleared goals are not restored.
- Works non-interactively (`claude -p "/goal ..."`), runs the loop to
  completion in one invocation; Ctrl+C interrupts.

## Comparison to related mechanisms

| Approach   | Next turn starts when     | Stops when                        |
|------------|---------------------------|-----------------------------------|
| `/goal`    | previous turn finishes    | a model confirms condition met    |
| `/loop`    | a time interval elapses   | user stops, or agent decides done |
| Stop hook  | previous turn finishes    | your own script/prompt decides    |

`/goal` adds a **separate evaluator** that checks the condition after every
turn, so completion is decided by a **fresh model**, not the one doing the work.
Auto-mode (approve tool calls) is complementary: auto-mode removes per-tool
prompts; `/goal` removes per-turn prompts.

## Implications for OUR Rust design (yaca)

This maps cleanly onto a **goal/verification engine**:

- **Goal state**: session-scoped struct `{ condition: String (≤4000),
  bound: Option<TurnOrTimeBound>, turns_evaluated, started_at, token_baseline,
  last_reason }`. One active goal per session.
- **Loop integration**: our agent loop's "turn finished" event triggers the
  evaluator. If `not met`, we auto-start another turn injecting `last_reason`
  as guidance. If `met`, clear + record.
- **Independent evaluator**: a separate model call (the cheap tier from D2's
  provider routing — Haiku/gpt-mini/local-small) that receives ONLY the goal
  condition + the recent transcript. It MUST NOT have tool access — matches the
  docs and is the whole point of an *independent* gate (no contamination from
  the main agent's reasoning/justification).
- **Transcript-only judging** is a design constraint, not a limitation to fix:
  it forces the main agent to surface evidence (run the command, show output),
  which is exactly the verification discipline we want.
- **Decoupling from team mode**: the goal gate sits ABOVE the orchestration
  loop. A goal turn may itself spin up a team; the gate only judges the final
  surfaced transcript. (Open question Q7: exact UX of how a user sets a goal
  that delegates to a team, and whether each team result must be summarized
  into the lead transcript for the gate to see it.)
- **Bound clause** prevents infinite loops — must implement turn/time caps as a
  hard safety even beyond the condition's own clause.

### Difference vs a plain Stop hook
We can implement the gate natively (not as an external hook) since we own the
loop, but keeping it as a pluggable "turn-end evaluator" trait keeps it
testable and lets users supply custom evaluators (script-based deterministic
checks vs model-based).
