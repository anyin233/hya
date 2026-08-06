# Fix the four known flaky tests

Follow-up from the 2026-08-05/06 E2E suite hardening round. Evidence lives in
`.trellis/tasks/archive/2026-08/08-05-land-swarm-branch-to-main/findings.md` and
`.trellis/tasks/archive/2026-08/08-05-ci-gate-e2e-tracks/findings.md`.

## Goal

Remove the four flaky tests that were observed during that round, so CI red
means something is actually broken.

## Why now

CI is a real gate as of `fee38938` — every step reports independently and Track P
is enforced. A gate people learn to ignore is worse than no gate, and four known
flakes is how that starts. Three of these were **invisible before** because the
steps that ran them were being skipped.

## The four, with what is actually known

Each entry states the evidence honestly, including how weak it is. Do not treat
a one-observation flake as a diagnosed one.

### 1. `recovered_promotions_reconstruct_each_parent_binding` (`crates/hya-app/src/runtime.rs`)

```
runtime.rs:6001: assertion `left == right` failed
  left: 0   right: 1        // resolve_recovered_admission_launches(...).len()
```

- Observed **once in two** full-workspace runs.
- Passes 6/6 in isolation and 5/5 as the full `hya-app --lib` suite.
- Reproduction condition: full `cargo test --workspace --jobs 1` under load, not
  the lib suite alone.
- Root cause **not established**. It was investigated only far enough to rule
  out the `runtime.rs:3410` guard landed in `16bde844` (the assertion is in
  `resolve_recovered_admission_launches`, which has no call relationship to
  `spawn_team_supervisor_with_environment`).

### 2. `missing_adjacent_launcher_reports_its_path` (`crates/hya/tests/frontend_cli.rs`)

```
Error: Os { code: 26, kind: ExecutableFileBusy, message: "Text file busy" }
```

- Observed **once**, on CI run 31055282111.
- ETXTBSY is the classic race of exec'ing a binary still held open for writing.
- Root cause **not established**. `frontend_cli.rs` runs 6 tests in parallel
  threads and at least one copies/execs a launcher binary — that is the place to
  look first, but it has not been confirmed.

### 3. `bundle_cli` process-id collisions (`crates/hya-backend/tests/bundle_cli.rs`)

- All 7 tests in the file share `std::process::id()` when building temp paths.
- `bundle_info_lists_prepared_static_resources` hit `AlreadyExists` on
  `fs::create_dir` (`bundle_cli.rs:406`) once in 24 whole-file runs.
- This one has a **plausible mechanism** (shared path derivation across
  parallel tests), unlike 1 and 2.

### 4. `pty-smoke.test.ts` — `timed out waiting for root draft`

```
packages/hya-tui-ts/test/pty-smoke.test.ts:589 — 64.97s
```

- Failed on CI run 31053432077 and passed on two other runs of **byte-identical
  code**; passes 3/3 locally.
- Already made non-gating in `fee38938` (`continue-on-error: true`), so it can
  no longer block the Rust gate. It still reports, and a permanently-red
  non-gating step is noise that trains people to ignore the whole column.

## Requirements

- R1. For each of the four, establish the root cause **or** state plainly that
  it could not be established and what was ruled out. A guess presented as a
  diagnosis is a worse outcome than an open item.
- R2. Fix what is diagnosed. Prefer removing the shared-state race over adding
  retries or raising timeouts — a retry converts a real intermittent bug into a
  slow one.
- R3. Prove each fix against the condition that reproduced the flake, not
  against a single green run. Where a reproduction recipe exists (items 1, 3),
  use it and report the before/after failure rate over a stated number of runs.
- R4. For anything that cannot be diagnosed, decide explicitly: quarantine with
  a recorded reason, or leave it and say why. Do not leave it undecided.
- R5. If `pty-smoke.test.ts` stays non-gating, say so in
  `docs/testing/agent-matrix.md` with the current status, so the red step is
  understood rather than ignored.

## Constraints

- These are timing- and load-dependent. A single green run proves nothing;
  state run counts for every claim.
- Item 1 has been seen only under full-workspace load — do not conclude it is
  fixed from lib-suite runs alone.
- Do not weaken an assertion to make a flake pass. That is the failure mode the
  previous round explicitly guarded against (see the `T2.6` receipt oracle,
  which looked green while proving nothing).

## Acceptance criteria

- [ ] Each of the four has a recorded root cause or an explicit "not
      established, here is what was ruled out".
- [ ] Diagnosed flakes are fixed, with before/after failure rates over a stated
      number of runs under the reproducing condition.
- [ ] Undiagnosed flakes have a recorded decision (quarantine + reason, or
      accept + reason).
- [ ] `cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast` green,
      and `cargo test -p hya-e2e -- --test-threads=1` green.
- [ ] No assertion was weakened to achieve a pass.

## Out of scope

- Any new test coverage.
- Restructuring the CI workflow (that landed in `fee38938`).
