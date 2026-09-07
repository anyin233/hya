# Implement — Make matrix.toml a driven registry

Land **after** child 3 (`08-05-e2e-swarm-tool-scenarios`), which is adding
scenarios and claiming `T2.4`–`T2.6`. Running before it would validate a
registry that is about to change.

## Step 1 — Read the live registry, do not trust this plan's snapshot

```sh
grep -c '^\[\[scenario\]\]' crates/hya-e2e/matrix.toml
grep '^id = ' crates/hya-e2e/matrix.toml | sort
ls crates/hya-e2e/tests/
```

**Check:** you have the current ID set and the current test files, including
anything child 3 added. The `T1.1` / `T1.6` / `T2.4`–`T2.6` gap list in
`design.md` was accurate on 2026-08-05 and may not be now.

## Step 2 — Add the `toml` dependency and the subcommand skeleton

- `crates/xtask/Cargo.toml`: add `toml = { workspace = true }`.
- `crates/xtask/src/main.rs`: add a `Some("matrix-check") => matrix_check::run(args.collect())`
  arm and mention it in the usage line.
- `crates/xtask/src/matrix_check.rs`: new module.

**Check:** `cargo run -p xtask -- matrix-check` runs and exits 0 (even as a
stub), and `cargo run -p xtask` still prints usage listing the new command.

## Step 3 — Parse and validate, in the order the checks are cheapest to trust

Implement per `design.md`:

1. every `path` exists (repo-root relative);
2. IDs unique and well-formed;
3. Track P forward drift — each entry's file holds ≥1 test function;
4. Track P reverse drift — each test function's file is referenced by ≥1 entry;
5. gap rule (step 5 below).

Correspondence is **file-level, not function-level** — `p01` carries two IDs in
one function, `p02` carries three, `p03` has one ID and two functions. A 1:1
rule would fail constantly and get disabled, which is worse than no checker.

**Check:** run against the current tree. It must exit **0**. If it does not,
the checker is wrong before the registry is — fix the checker first.

## Step 4 — Prove it fails, in both directions (load-bearing)

A checker that has never failed is not known to work.

```sh
# phantom entry: add a [[scenario]] with a nonexistent path → expect non-zero exit
# unregistered test: add a dummy #[tokio::test] fn in a NEW unreferenced file → expect non-zero exit
```

Revert both probes afterwards.

**Check:** both probes produce a non-zero exit and a message naming the specific
offending id/path. Record the exact output in this task — that evidence is the
deliverable, not the code.

## Step 5 — Resolve the numbering gaps

Add a `[[retired]]` table to `matrix.toml` for every ID in a track's numeric
range that is neither used nor already retired, each with a real `reason`
(not "unused"). Then make the checker enforce: used ∪ retired must cover the
range, with no holes.

**Check:** removing one `[[retired]]` entry makes the checker fail. Verify, then
restore.

## Step 6 — Document the allocation rule

Update `docs/testing/agent-matrix.md` §"Adding a scenario": new IDs take the
next free number; retiring one requires a `[[retired]]` row with a reason;
`cargo xtask matrix-check` enforces both.

## Step 7 — Wire into CI

Add to `.github/workflows/ci.yml`, carrying `if: ${{ !cancelled() }}` like every
other gate step child 2 established:

```yaml
- name: matrix registry check
  if: ${{ !cancelled() }}
  run: cargo run -p xtask -- matrix-check
```

**Check:** `cargo fmt --all --check`, `cargo clippy -p xtask --all-targets -- -D warnings`,
and the workflow still parses.

## Step 8 — Full verification and land

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- matrix-check
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
```

Push; CI enforces the gate, so the run is the final check.

## Rollback

Revert the xtask module, its `Cargo.toml` line, the CI step, and the
`[[retired]]` block. The registry returns to inert prose — no runtime impact.

## Do not

- Turn this into a scenario executor.
- Map registry rows to individual test functions.
- Guess at `reason` text for retired IDs — if the history is unclear, say so in
  the reason rather than inventing a rationale.
