# Implement — Wire E2E tracks into CI gate

Execution plan. Step 3 is a decision gate; step 6 is the load-bearing
verification and must not be skipped.

## Step 1 — Baseline the current CI cost

Before changing anything, record what the workflow costs today, so the added
Track P step can be judged against a real number (PRD constraint).

```sh
gh run list --branch main --limit 5 --json databaseId,conclusion,createdAt,updatedAt
gh run view <latest-green-run> --json jobs -q '.jobs[] | "\(.name) \(.startedAt) \(.completedAt)"'
```

**Check:** total wall-clock of the last green run recorded in this task.
Reference points: run 31053324678 and 31053472545 on `main` are both fully
green, so either is a valid baseline.

## Step 2 — Confirm the environment assumptions Track P needs on CI

PRD constraint: `p06_mcp` spawns a Python-based MCP fixture, and Track P binds
`127.0.0.1:0`.

```sh
grep -rn "python" crates/hya-e2e/src/backend.rs | head
```

**Check:** confirm whether the fixture invokes `python3` explicitly (child 1's
preflight reported it does, and that bare `python` is absent on the runner but
never used). If it uses bare `python`, that is a blocker to fix before Track P
can be a gate — record it and stop.

## Step 3 — DECISION GATE: `cargo clean` vs `build`

Per `design.md`, steps 11 (`cargo build --workspace`) and 12 (`cargo clean`)
cancel each other out: the clean deletes exactly what the build produced, and
the test step then rebuilds from scratch.

Present to the user: keep `cargo clean` and drop the redundant `build` step, or
drop `cargo clean` and let `build` do useful work. Do **not** choose
unilaterally — this changes CI runtime characteristics and may have been
deliberate.

## Step 4 — Edit `.github/workflows/ci.yml`

Apply, in one edit:

1. Add `if: ${{ !cancelled() }}` to every gate step from `fmt` onward
   (`fmt`, `clippy`, `build`, `test`, the new Track P steps, `install strace`,
   `verify-no-http`).
2. Replace the blanket `bun test` with the three registered Track T scenarios:
   ```
   bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
   ```
3. Add a separate non-gating step for the rest of the bun suite, PTY included:
   ```yaml
   - name: TUI smoke (non-gating)
     continue-on-error: true
     working-directory: packages/hya-tui-ts
     run: bun test
   ```
4. Change the workspace test to `cargo test --workspace --jobs 1 --exclude hya-e2e`.
5. Add the Track P steps:
   ```yaml
   - name: build agent e2e binaries
     if: ${{ !cancelled() }}
     run: cargo build --locked -p hya-backend --bin hya-backend
   - name: agent process e2e (Track P)
     if: ${{ !cancelled() }}
     run: cargo test -p hya-e2e -- --test-threads=1
   ```
6. Apply the step-3 decision about `cargo clean` / `build`.

**Check:** `ci.yml` parses — `gh workflow view ci` or a YAML lint. No
`continue-on-error` on any *gate* step (only on the smoke step).

## Step 5 — Update the docs so they match reality

- `docs/testing/README.md` — the gate is real; drop "optional".
- `docs/testing/agent-matrix.md` — state that the three Track T scenarios are
  enforced and PTY smoke is explicitly non-gating (this now matches the
  workflow instead of contradicting it).
- `docs/testing/ci-agent-e2e-snippet.yml` — delete it, or reduce it to a pointer
  at `ci.yml`. It must not keep claiming the wiring is unapplied.

## Step 6 — VERIFY THE CASCADE FIX (load-bearing — do not skip)

The entire point of this task is that one failing step no longer hides another.
That must be *observed*, not assumed.

On a scratch branch:

1. Deliberately break an early gate step (e.g. introduce a formatting error so
   `fmt` fails).
2. Push the scratch branch and let CI run.
3. **Confirm the later steps still ran and reported** — specifically that
   `test` executed and reported its own result while `fmt` was red.
4. Delete the scratch branch.

**Check:** a CI run exists where `fmt` = failure **and** `test` = success (or
failure on its own merits) rather than `skipped`. Record the run ID in this
task. Without this evidence the task is not done.

## Step 7 — Land and confirm

```sh
git push origin main   # after the usual local gate
gh run watch <run-id> --exit-status
```

**Check:** all gate steps report; the run's conclusion reflects the real state.

## Step 8 — Record the cost delta

Compare against step 1's baseline and record the added wall-clock in this task
(PRD constraint). If Track P materially lengthens CI, propose — do not adopt —
splitting into parallel jobs (`design.md` option A).

## Rollback

Single-file revert of `.github/workflows/ci.yml` plus the docs commit. No
runtime or data impact. The scratch branch from step 6 is deleted either way.

## Known trap — do not recreate it

`fmt` used to run before `test` and abort the job, which is how six broken tests
shipped unnoticed for weeks (see
`.trellis/tasks/08-05-land-swarm-branch-to-main/findings.md`). Any future edit
that reintroduces "cheap check first, and it aborts everything" recreates the
exact defect this task exists to remove.
