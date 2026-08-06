# Design — Wire E2E tracks into CI gate

## The real problem

The naive reading of this task is "add two steps to `ci.yml`". That would be
wrong. Child 1 produced evidence that the workflow's *structure* is the defect:

`.github/workflows/ci.yml` is a **single job with 15 linear steps**. GitHub
Actions skips every subsequent step once one fails. That has hidden real
defects twice, three weeks apart:

| Incident | Failing step | What got skipped |
| --- | --- | --- |
| `fmt` red on `main` for weeks | 9 (`fmt`) | clippy, build, **test**, verify-no-http |
| PTY flake after the push | 8 (`Test TypeScript TUI`) | fmt, clippy, build, **test**, verify-no-http |

In the first case the skipped `test` step was hiding **six failing tests, three
of them regressions**. Nobody could see them, because CI reported a `fmt`
failure and stopped.

So the design goal is: **no gate step may prevent another gate step from
reporting.**

## Current workflow, annotated

```yaml
jobs:
  check:                              # ← one job, everything serialized
    steps:
      5.  Install Bun
      6.  Prepare TypeScript TUI      # bun install / typecheck / build
      7.  Build TypeScript TUI test binaries   # cargo build -p hya -p hya-backend -p hya-ts --bins
      8.  Test TypeScript TUI         # bun test  ← blanket; runs PTY smoke; gates everything below
      9.  fmt
      10. clippy
      11. build                       # cargo build --workspace
      12. clean build artifacts       # cargo clean  ← discards step 11 entirely
      13. test                        # cargo test --workspace --jobs 1  (includes hya-e2e)
      14. install strace
      15. verify-no-http
```

Three defects beyond the cascade:

- **Step 12 negates step 11 — but deliberately.** `cargo clean` deletes the
  artifacts step 11 just produced, so step 13 rebuilds from scratch. My first
  reading of this was "accidental waste, remove it". That was **wrong**:
  `git log -L` shows it was added by `ff70f461 "ci: reduce test linking
  pressure"`. GitHub runners are disk-constrained, and a full workspace build
  plus test artifacts can exhaust them. Removing it risks
  `no space left on device`.

  **Decision: leave both steps alone.** They are not the defect this task
  exists to fix, and changing them trades a 25s saving for a real
  disk-exhaustion risk. Recorded here so the next person does not re-derive
  the same wrong conclusion. (Superseded the "flag to the user" note in the
  target-shape section below.)
- **Step 13 runs `hya-e2e` in the configuration its own docs call unstable.**
  `--jobs 1` bounds codegen parallelism, not test threads, so `p03`, `p11`,
  `p14`, `p15` each run 2 tests — and 2 backends — concurrently.
  *Honesty note:* this configuration **passed** on runs 31053324678 and
  31053472545. The instability is a real risk, not an observed failure. Do not
  justify the change by claiming CI is currently broken by it; justify it by the
  harness contract in `docs/testing/process-e2e.md`.
- **Step 8 runs a blanket `bun test`,** which executes `pty-smoke.test.ts`.
  `docs/testing/agent-matrix.md` explicitly says PTY is *not* required for the
  PR gate — yet it gates everything. The docs and the workflow disagree, and the
  workflow wins.

## Options considered for the cascade

**A. Split into independent jobs** (`rust`, `e2e`, `ts`, `pty`).
Cleanest isolation and they run in parallel. But every job pays its own
`cargo` build: the TS job needs `-p hya -p hya-backend -p hya-ts --bins`, the
e2e job needs the `hya-backend` bin, the rust job needs the workspace. With
`Swatinem/rust-cache` restoring per job this is survivable but roughly doubles
compute, and cache restore of a workspace this size is not free.

**B. Keep one job, add `if: ${{ !cancelled() }}` to every gate step.**
Each step runs and reports regardless of earlier failures; the job still fails
if any step failed. Near-zero cost, no cache duplication, no build repetition.
Downside: a broken `build` makes `test` fail noisily too — acceptable, since it
fails fast and the true first cause is still visible in the step list.

**Chosen: B, plus pulling PTY out of the gate.** It fixes the observed failure
class at the lowest cost. Option A stays on the table if the job ever grows long
enough that parallelism matters — record that, do not pre-emptively build it.

`if: ${{ !cancelled() }}` rather than `if: always()`: `always()` would keep
running steps after a manual cancellation, which wastes runner time and confuses
the cancel button.

## Target shape

```yaml
      8.  Test TypeScript TUI (Track T)   # the 3 registered scenarios only
      8b. PTY smoke                       # continue-on-error: true — reports, never gates
      9.  fmt                             # if: !cancelled()
      10. clippy                          # if: !cancelled()
      11. build                           # (see below)
      12. test (workspace, --exclude hya-e2e)   # if: !cancelled()
      13. build hya-backend bin           # if: !cancelled()
      14. Track P: cargo test -p hya-e2e -- --test-threads=1   # if: !cancelled()
      15. verify-no-http                  # if: !cancelled()
```

Decisions embedded above:

- **Track T's enforced set becomes explicit**: `real-backend.test.ts`,
  `task-presentation.test.ts`, `real-backend-agents.test.ts` — exactly the three
  registered in `matrix.toml`. This makes the workflow agree with the docs
  instead of silently exceeding them.
- **PTY smoke keeps running** but with `continue-on-error: true`. It is genuine
  signal; it is just not a gate, which is what the docs already say. Losing the
  signal entirely would be worse than a noisy non-blocking step.
- **`cargo clean` and `build` are left untouched.** See the annotated-workflow
  section above: `cargo clean` exists to relieve disk/linking pressure
  (`ff70f461`), so removing it risks exhausting the runner. Out of scope.

## Risks

| Risk | Mitigation |
| --- | --- |
| `!cancelled()` makes a cascade of noisy failures hard to read | The step list still shows the first failure; the summary lists all. Net improvement over silence. |
| Track P added as its own step lengthens CI | Measure and record. It took 13s locally warm; on CI after `cargo clean` it also needs the backend built. |
| Narrowing `bun test` to three files loses coverage of the other 8 test files | Real trade-off. Those files are unit/presentation tests that `agent-matrix.md` does not register. Prefer running them in the non-gating step alongside PTY rather than dropping them. |
| Editing `.github/workflows/` needs `workflow` scope on push | Known from the pre-existing snippet's caveat; parent PRD decision **D2** accepted editing it directly, and child 1 already pushed workflow-adjacent commits successfully. |

## Verification strategy

A CI change cannot be fully verified locally. The plan is:

1. Reproduce the exact command sequence locally first (child 1 already proved it
   green: 1324 passed / 0 failed, Track P 19/19, Track T 50/50).
2. Prove the cascade fix by deliberately breaking one step on a scratch branch
   and confirming the *later* steps still run and report.
3. Only then land on `main`.

Step 2 is the load-bearing verification — without it, this task would be
asserting a behavior change it never observed.
