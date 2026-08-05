# Wire E2E tracks into CI gate

Child 2 of `08-05-e2e-suite-hardening`. Closes Gap 1. Depends on child 1
(`08-05-land-swarm-branch-to-main`).

## Goal

Make Track P (process E2E) and the non-PTY Track T scenarios a real, enforced
part of the PR gate, run in the configuration the harness docs require.

## Why this exists

`.github/workflows/ci.yml` is byte-identical on `main` and the feature branch
and contains no e2e step. The only wiring that exists is a fully commented-out
fragment at `docs/testing/ci-agent-e2e-snippet.yml`, gated behind a
"when the token has workflow write scope" caveat. Parent PRD decision **D2**
resolves that caveat: edit `ci.yml` directly.

The current `cargo test --workspace --jobs 1` step does compile and run
`hya-e2e`, but `--jobs 1` bounds codegen parallelism, not test threads. Files
`p03_permissions`, `p11_hyabundle`, `p14_compact_summarize`, and
`p15_todo_and_edit` each hold 2 test functions that therefore run concurrently,
spawning concurrent backend processes — precisely what
`docs/testing/process-e2e.md` says is unstable and requires `--test-threads=1`.
So today's CI both fails to gate the suite deliberately and runs it wrong.

## Evidence from child 1 (2026-08-05) — this changes the requirements

Child 1 landed the suite and, in doing so, produced hard evidence about how this
workflow actually fails. Full detail in
`.trellis/tasks/08-05-land-swarm-branch-to-main/findings.md`.

**The workflow is one job with a linear step list, so any early failure skips
everything after it.** That is not hypothetical — it has hidden real defects
twice:

1. **`fmt` (step 9) had been failing on `main` for weeks.** Every step after it
   — `clippy`, `build`, `test`, `verify-no-http` — was skipped on every run. Six
   broken tests, three of them regressions, shipped completely unnoticed because
   the test step never executed. Child 1 found them only by running the gate
   locally.
2. **A PTY test the docs call non-gating blocks the entire Rust gate.** After
   the push, CI failed at **step 8, "Test TypeScript TUI"**, on
   `test/pty-smoke.test.ts` (`timed out waiting for root draft`, 64.97s) — and
   `fmt`, `clippy`, `build`, `test`, `verify-no-http` were all skipped again.
   Proven flaky: the identical code passed that step in two other runs, and
   passes 3/3 locally.

Meanwhile `docs/testing/agent-matrix.md` says: *"Full PTY matrix for every
feature ID is **not** required for the PR gate; PTY remains presentation
smoke."* The blanket `bun test` at step 8 contradicts that doc — it runs PTY
smoke and lets it gate everything.

**So the real requirement is to fix the failure class, not to append two steps
to a fragile linear list.** A gate where one flaky, explicitly-non-essential
test can prevent the entire Rust suite from running is not a gate.

## Requirements

- R1. Exclude `hya-e2e` from the generic workspace test step and run it as a
  dedicated step with `--test-threads=1`, after an explicit
  `cargo build -p hya-backend --bin hya-backend`.
- R2. Run the three registered Track T scenarios (`real-backend.test.ts`,
  `task-presentation.test.ts`, `real-backend-agents.test.ts`) as an enforced
  step. Confirm whether the existing blanket `bun test` step already covers
  them; if it does, make that coverage explicit rather than adding a duplicate
  step.
- R3. A failing Track P or Track T scenario must fail the workflow. No
  `continue-on-error`, no soft-warning step.
- R4. Keep the `cargo clean` step's intent intact: the backend binary Track P
  needs must be built after any clean, not assumed present.
- R5. Update `docs/testing/README.md`, `docs/testing/agent-matrix.md`, and
  `docs/testing/ci-agent-e2e-snippet.yml` so they describe the gate that now
  exists. The snippet file is either deleted or reduced to a pointer — it must
  not keep claiming the wiring is optional and unapplied.
- R6. **Decouple the steps so one early failure cannot skip the rest.** The Rust
  gate must run even when the TypeScript/PTY steps fail, and vice versa. Split
  into independent jobs (or otherwise ensure independent steps report
  independently) so a red `fmt` never again hides a red `test`. This supersedes
  a naive "append two steps" reading of R1/R2.
- R7. **Make Track T's enforced set explicit.** Replace the blanket `bun test`
  with the three registered Track T scenarios (`real-backend.test.ts`,
  `task-presentation.test.ts`, `real-backend-agents.test.ts`), matching what
  `agent-matrix.md` already documents as the PR requirement. PTY smoke may still
  run, but must not gate the Rust suite — either in a non-blocking job or
  quarantined until its flakiness is fixed.
- R8. **Decide and record what happens to `pty-smoke.test.ts`.** It is
  demonstrably flaky under CI load. Options: quarantine, retry, raise its
  timeout, or fix the underlying wait. Do not leave it silently gating.

## Constraints

- CI wall-clock is a real cost: `--test-threads=1` serializes 20 scenarios, and
  the workflow already does a full `cargo clean` before tests. Measure the added
  time and record it; if it is severe, propose (do not unilaterally adopt) a
  separate job so Track P runs in parallel with the rest.
- `RUSTFLAGS: "-D warnings …"` is workflow-global; new steps inherit it.
- Track P spawns real processes and binds `127.0.0.1:0`. Confirm this works
  under `ubuntu-latest` as configured, including the Python-based MCP echo
  fixture used by `p06_mcp` — the runner must actually have `python`.

## Acceptance criteria

- [ ] `.github/workflows/ci.yml` contains a dedicated Track P step running
      `cargo test -p hya-e2e -- --test-threads=1`, preceded by a backend build.
- [ ] The generic workspace test step no longer runs `hya-e2e`
      (`--exclude hya-e2e`), so no scenario runs multi-threaded.
- [ ] Track T's three registered scenarios are demonstrably enforced.
- [ ] A deliberately broken scenario fails the workflow — verified locally by
      running the exact CI command sequence against a temporarily broken
      assertion, then reverting it.
- [ ] `p06_mcp` passes in the CI environment (Python fixture available).
- [ ] Added CI wall-clock time is measured and recorded in this task.
- [ ] No `docs/testing/` page still describes Track P as optional or unwired.

## Out of scope

- Adding new scenarios (child 3 owns that).
- Coverage collection in CI (child 4 owns the baseline; whether it ever becomes
  a CI step is that task's call).
- Restructuring the workflow into a multi-job matrix, unless R2's timing
  measurement forces the question — in which case propose, don't adopt.
- PTY (`pty-smoke.test.ts`) enforcement.

## Rollback

Single-file revert of `.github/workflows/ci.yml` plus the docs commit. No
runtime or data impact.
