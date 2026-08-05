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
