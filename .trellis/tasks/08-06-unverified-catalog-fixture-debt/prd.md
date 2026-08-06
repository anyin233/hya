# Retire unverified catalog test fixtures

Follow-up from the E2E hardening round. The contract this task defends is
recorded in `.trellis/spec/backend/task-tool.md` §"Test Fixtures That Route
Through Durable Spawn Admission".

## Goal

Stop the next test that routes a spawn through durable admission from hitting
the same wall that silently broke `nested_spawn_tree.rs` for weeks, and remove
the duplicated fixture helper that caused it.

## Why this exists

On 2026-08-05 three tests in `crates/hya-app/tests/nested_spawn_tree.rs` failed
with an opaque `SpawnError::Unavailable`. Root cause: their fixture built the
catalog with `BundleCatalog::from_prepared`, which deliberately leaves
`semantic_identity_v1: None`, so durable admission cannot compute a runtime
fingerprint and fails closed. A second, independent gap sat behind it: a bare
`FakeProvider` leaves `configured_identity_v1` at the fail-closed default.

Both are **asserted product behaviours**, so the tests were stale, not the
product. Fixing them required a verified catalog *and* a provider identity.

The mechanism that let it rot: commit `994ea6b2` added a verified-runtime helper
for `spawn_admission.rs` but never backfilled the shared
`crates/hya-app/tests/support/mod.rs`. Two helpers, one updated, one not.

## Current state, measured 2026-08-06

- `BundleCatalog::from_prepared` has **99 call sites across 15 files** outside
  `crates/hya-bundle/src/catalog.rs`. The "six fixtures" figure recorded during
  the previous round was wrong — it was a spot count, not a survey.
- Most of those are in `crates/hya-core/src/runtime_registry.rs` unit tests that
  never route a spawn through `prepare_spawn_admission`, so they are fine today.
  **The risk applies only to fixtures that could reach durable admission**, and
  identifying that subset is the first real task here.
- `verified_admission_test_runtime` (`crates/hya-app/tests/spawn_admission.rs`)
  and `support::test_runtime` (`crates/hya-app/tests/support/mod.rs`) are now
  **byte-identical**, in the same directory, and `spawn_admission.rs` already
  declares `mod support;`. The duplication that caused the original rot is still
  present.

## Requirements

- R1. Survey the 99 call sites and classify each as *can* or *cannot* reach
  `prepare_spawn_admission`. Report the counts. Only the former are debt.
- R2. Migrate the reachable ones to a verified catalog. Leave the rest alone and
  say why — churning 99 sites to fix a handful would be its own kind of damage.
- R3. Dedupe: delete `verified_admission_test_runtime` and have
  `spawn_admission.rs` call `support::test_runtime`. This is the specific fix
  for the drift mechanism, not a cosmetic tidy-up.
- R4. Move `IdentityFakeProvider` (`nested_spawn_tree.rs`) into
  `tests/support/` alongside it, for the same reason. Do **not** add
  `configured_identity_v1` to `FakeProvider` itself —
  `crates/hya-app/src/runtime.rs:5016-5026` asserts a bare-`FakeProvider` router
  must fail closed, and the fake is used across many crates.
- R5. `support::test_runtime` builds its `bundle.yaml` by string concatenation.
  An agent id that is a YAML 1.1 bareword boolean (`no`, `on`, `y`) or otherwise
  needs quoting would produce a confusing prepare error. Either quote properly
  or assert loudly on ids that would need it.
- R6. `crates/hya-app/tests/spawn_admission.rs:1681` awaits `queued_spawn`
  without a timeout, unlike its siblings. A non-replying supervisor hangs the
  whole test binary instead of failing. Bound it.

## Constraints

- Every migrated fixture must still pass. Some tests deliberately use an
  *unverified* catalog to assert fail-closed behaviour — `spawn_admission.rs`
  publishes a replacement catalog unverified on purpose and asserts the old
  binding stays pinned. Migrating that one would destroy the test.
- Two of the remaining sites encode per-agent `harness_access`, which the
  current manifest helper's `(&str, AgentRole, &[&str])` signature cannot
  express. Migrating them is real work, not a rename — scope it or defer it
  explicitly.
- Existing mitigation worth preserving: `spawn_admission.rs:1636` asserts
  `quick.prompt == "quick prompt"`, so helper drift fails loudly rather than
  silently.

## Acceptance criteria

- [ ] Survey published: how many of the 99 sites can reach durable admission,
      and which files they are in.
- [ ] Reachable fixtures migrated to a verified catalog; unreachable ones left
      alone with a stated reason.
- [ ] Exactly one verified-runtime helper exists, in `tests/support/`.
- [ ] `IdentityFakeProvider` lives in `tests/support/`; `FakeProvider` itself is
      unchanged.
- [ ] YAML id hazard either fixed or asserted against.
- [ ] `spawn_admission.rs:1681` has a bounded wait.
- [ ] `cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast` and
      `cargo test -p hya-e2e -- --test-threads=1` both green.

## Out of scope

- Changing `BundleCatalog::from_prepared` itself or the fail-closed contract.
  Both are deliberate and asserted (`crates/hya-bundle/tests/catalog.rs:166`).
- New scenarios.
