# Make matrix.toml a driven registry

Child 5 of `08-05-e2e-suite-hardening`. Closes Gap 4. Depends on child 1;
child 3 adds scenario IDs this checker must accept.

## Goal

Make `crates/hya-e2e/matrix.toml` load-bearing: an automated check that fails
when the registry and the actual tests disagree, so scenario IDs cannot drift
into fiction.

## Why this exists

`matrix.toml` describes itself as the "machine registry" and
"machine-readable PR-matrix registry", but a repo-wide grep finds **zero**
consumers — no runner, no test, no CI step reads it. It is prose in TOML
clothing. Consequences already visible today:

- IDs `T1.1`, `T1.6`, `T2.4`, `T2.5`, `T2.6` appear nowhere in the repo — not
  in the registry, not in any PRD, not in `docs/`. The numbering has holes and
  nothing tracks whether they are unimplemented scenarios or retired ones.
- Every entry's `path` and `timeout_secs` is unverified. A renamed or deleted
  test file leaves a stale row that still reads as coverage.

## Requirements

- R1. Add an automated check (an `xtask` subcommand is the natural home —
  `crates/xtask` already hosts repo tooling like `sync_compat`) that validates
  `matrix.toml` against reality:
  - every `path` exists;
  - every Track P entry maps to at least one real `#[tokio::test]` /
    `#[test]` function in that file;
  - every Track P test function is registered by at least one entry
    (drift detection in both directions — an unregistered test is as much a
    registry failure as a phantom entry);
  - IDs are unique and well-formed.
- R2. The check runs in CI and fails the build on drift. Coordinate with child 2
  so this becomes part of the same gate rather than a second, competing one.
- R3. Resolve the numbering gaps: define `T1.1`, `T1.6`, `T2.4`–`T2.6` as real
  scenarios, or formally retire them with a recorded reason. Whichever is
  chosen, the registry must state it so the holes stop being ambiguous.
- R4. Document the ID allocation rule so future scenarios cannot reintroduce
  silent gaps, and update `docs/testing/agent-matrix.md` §"Adding a scenario"
  to reference the check.

## Constraints

- Mapping registry rows to test functions is many-to-many: `p01_session_prompt.rs`
  carries both `T0.1` and `T1.2` in a single test function, while
  `p02_tool_loop_fs.rs` carries `T1.3`–`T1.5`. The check must model this
  honestly rather than assuming one row per function.
- Track I entries are index-only pointers into other crates' tests and Track T
  entries point at TypeScript files. The check must handle all three tracks —
  Track T rows cannot be validated by scanning Rust test attributes.
- `matrix.toml` currently registers `packages/hya-tui-ts/test/real-backend-agents.test.ts`;
  verify it exists before making its absence a build failure.
- Parsing test functions by regex is fragile. Prefer a check that is
  conservative and clearly-scoped over one that is clever and wrong; a false CI
  failure here erodes trust in the whole gate.

## Acceptance criteria

- [ ] A runnable check exists (e.g. `cargo xtask matrix-check`) and is
      documented.
- [ ] It fails on a phantom entry — verified by temporarily adding a row with a
      nonexistent path.
- [ ] It fails on an unregistered test — verified by temporarily adding a test
      function with no matching entry.
- [ ] It passes on the tree as landed, including any scenarios added by child 3.
- [ ] `T1.1`, `T1.6`, `T2.4`–`T2.6` are each either defined or explicitly
      retired in `matrix.toml`, with a reason.
- [ ] The ID allocation rule is documented in `docs/testing/agent-matrix.md`.
- [ ] The check runs in CI and its failure fails the workflow.
- [ ] `cargo clippy -p xtask --all-targets -- -D warnings` clean.

## Out of scope

- Turning `matrix.toml` into a test *executor* (selecting and running scenarios
  by tag or ID). Validation first; execution is a separate, larger question.
- Enforcing `timeout_secs` at runtime.
- Backfilling the retired IDs as actual scenarios — R3 only requires a decision
  and a record, not implementation.

## Rollback

Additive tooling plus a CI step. Revert the xtask module and the CI step; the
registry file reverts to inert prose.
