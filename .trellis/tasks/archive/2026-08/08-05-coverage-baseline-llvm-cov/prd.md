# Establish line coverage baseline

Child 4 of `08-05-e2e-suite-hardening`. Closes Gap 3. Depends on child 1.

Lightweight task: PRD-only.

## Goal

Produce a reproducible line-coverage number for the workspace, and a documented
command that regenerates it, so "coverage" stops meaning "a list of scenario
titles" and starts meaning measured data.

## Why this exists

The repository has a scenario inventory (`matrix.toml`, `agent-matrix.md`) but
no code-level coverage data at all. Neither `cargo-llvm-cov` nor
`cargo-tarpaulin` is installed, and no CI step collects coverage. Every
statement about how much of the codebase is exercised is currently an inference
from test titles.

## Requirements

- R1. Install `cargo-llvm-cov` (preferred over tarpaulin: LLVM source-based
  instrumentation, better Rust 2024 / workspace support).
- R2. Produce a workspace line-coverage report, broken down per crate.
- R3. Produce a second measurement that isolates Track P's contribution — what
  the process E2E suite covers on its own versus the in-process suites. This is
  the number that actually informs where to invest next.
- R4. Record both results as a dated baseline artifact in the repo (a
  `docs/testing/coverage-baseline.md` or equivalent), including the exact
  command, toolchain version, and date.
- R5. Document the regeneration command in `docs/testing/README.md`.

## Constraints

- Coverage instrumentation plus a full workspace build is expensive; this is a
  measurement task, so budget for a long run and do not compete with other
  builds using the same `target/` directory.
- Track P spawns a **separate backend process**. Standard in-process coverage
  instrumentation will not capture the child process unless `LLVM_PROFILE_FILE`
  is propagated and the backend binary is itself built with instrumentation.
  R3 depends on getting this right — if it proves infeasible within reasonable
  effort, report that honestly with evidence rather than publishing a number
  that silently undercounts Track P to near-zero.
- Do not chase a coverage target. This task establishes a baseline; it does not
  add tests to move it.

## Acceptance criteria

- [ ] `cargo-llvm-cov` installed and its version recorded.
- [ ] Workspace line coverage measured, with a per-crate breakdown.
- [ ] Track P's isolated contribution measured — or a written, evidenced
      explanation of why cross-process coverage could not be captured, plus the
      option that was rejected and why.
- [ ] Baseline artifact committed with command, toolchain version, and date.
- [ ] Regeneration command documented in `docs/testing/README.md` and verified
      by running it from a clean checkout state.
- [ ] The parent PRD's "Line coverage: never measured" baseline row is updated
      to the real number.

## Out of scope

- Adding a coverage gate or threshold to CI. Wiring coverage into CI is a
  follow-up decision that needs the baseline number first.
- Uploading to a coverage service (Codecov and similar).
- Writing tests to raise the number.

## Rollback

Measurement plus documentation only; no production code touched. Revert the
docs commit.
