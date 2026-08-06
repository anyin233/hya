# Fix plan — 6 failing tests on merged main

Produced 2026-08-05 by a 7-agent diagnosis workflow (3 investigators + 3 adversarial
reviewers + synthesis). Every root cause was proven by an executed control experiment
and independently re-derived by a reviewer. Verbatim synthesis follows.

---

# ORDERED FIX PLAN — 6 failing tests on merged main (620611cc)

## 0. Verdict summary

| # | Failure | Cluster | Classification | Root cause status |
|---|---|---|---|---|
| 1 | `hya-backend::bundle_cli::private_info_is_opaque_and_install_does_not_mutate_registry` | 3 | **stale test** | **PROVEN** |
| 2-4 | `hya-app::nested_spawn_tree` ×3 | 1 | **stale test fixture** (two gaps) | **PROVEN** |
| 5 | `hya-app::spawn_admission::admitted_background_transient_releases_its_exact_debit_on_completion` | 2 | **stale test** (lost implicit sync) | **PROVEN** |
| 6 | `hya-app::spawn_admission::queued_spawn_uses_parent_turn_binding_after_catalog_publication` | 2 | **product defect, but in a branch that is unreachable in production** | trigger **PROVEN**; the "this is a live production bug" framing is **DISPROVEN** |

**All four root causes are proven**, each by an executed control experiment (not by reading alone), and each was independently re-derived by an adversarial reviewer. Nothing here is a ranked hypothesis. The one thing that is *not* settled is a **judgement call**, not a fact: for failure #6, whether the correct fix is product-side or test-side (see Fix 4).

**The three clusters are independent — three separate fixes are needed, four edits total.** This was tested, not assumed:
- Cluster 2's owner re-linked the *unmodified* `nested_spawn_tree.rs` against a lib carrying the cluster-2 product fix: still `0 passed; 3 failed`, same panic site.
- Cluster 1's owner instrumented every `SpawnError::Unavailable` return site in `prepare_spawn_admission` and ran the two cluster-2 failures: **zero** probes fired — they never reach that function.
- Cluster 3 is a version literal in a different crate with no coupling to spawn admission.

The only shared cause is the **meta-cause**: CI died at `fmt` before `test` on both branches, so none of these ever ran. (Note: `cargo fmt --check` at HEAD in this repo exits **0** — I verified. The mismatch a reviewer reported for `use hya_server::{AppState, router}` does **not** reproduce here; it is an artifact of formatting a scratch copy that lacked the repo's `rustfmt.toml` (`edition = "2024"`, `max_width = 100`). Always run `cargo fmt` from the repo root.)

---

## Fix 1 — `bundle_cli` version literal (do first: zero risk, unblocks a dead test half)

**File:** `/chivier-disk/yanweiye/Projects/yaca/crates/hya-backend/tests/bundle_cli.rs`, line 155

```rust
-        "activation: unsupported-in-0.34.11",
+        concat!("activation: unsupported-in-", env!("CARGO_PKG_VERSION")),
```

**Why correct.** The product line `crates/hya-backend/src/bundle_cmd.rs:123` prints `env!("CARGO_PKG_VERSION")` and has **never** been anything else — `git log -L 123,123:crates/hya-backend/src/bundle_cmd.rs` shows exactly one commit (63c5c7b0), the one that introduced it. The test literal was hand-written twice (0.34.10, then 0.34.11) and then abandoned across two release bumps (142863fa → 0.34.12, 156f9bb8 → 0.34.13), neither of which touched the test. Workspace version is `Cargo.toml:9 = 0.34.13`; the test crate and the binary under test both use `version.workspace = true`, so `concat!(env!(...))` can never drift again. It matches the pattern already used by `crates/hya/tests/frontend_cli.rs:88`, `crates/hya-ts/tests/process.rs:32`, `crates/hya-server/tests/compat_session_api.rs:519`. `concat!` yields a `&'static str`, so the surrounding array/loop is unchanged.

**Do NOT touch `bundle_cmd.rs:123`.** The versioned banner is documented intent (`docs/changes/CHANGELOG_0.34.10.md:12`).

**Verify:** `cargo test -p hya-backend --test bundle_cli`

**⚠ Assertion-change flag (justified).** This edits a test assertion, so it deserves scrutiny — and it passes it: the two invariants the test name advertises both **hold today**. `authentication: unverified`, `payload: opaque`, and the strict "no ciphertext lines beyond length+digest" equality all pass. The failing line is a cosmetic banner. Additionally, the panic at line 157 has made lines 174-198 (the whole install/registry half) **dead since 0.34.12**; a reviewer executed that half for real against the authentic on-disk fixture with the real `hya_store::BundleRegistry` and got `SECOND HALF: ALL ASSERTIONS PASSED` (install exits 1 with `PRIVATE_ACTIVATION_UNSUPPORTED`, generation stays 0). So this fix un-blocks genuinely-passing coverage rather than hiding a second failure.

---

## Fix 2 — `nested_spawn_tree` fixture, part A: verified catalog (test-only)

**File:** `/chivier-disk/yanweiye/Projects/yaca/crates/hya-app/tests/support/mod.rs`, whole body of `test_runtime`

Replace the hand-built `PreparedBundle` + `BundleCatalog::from_prepared(&[bundle])` with the verified construction that already exists at `crates/hya-app/tests/spawn_admission.rs:44-77` (`verified_admission_test_runtime`): synthesize a `bundle.yaml` manifest + `prompts/<id>.md` files → `hya_bundle::prepare_builtins(vec![BundleSource::new("hya/app-tests", files)])` → `BundleCatalog::from_verified_catalogs(&[&prepared])` → `RuntimeRegistry::from_snapshot(tools.snapshot(), Arc::new(catalog))`. Manifest must reproduce the current values: `spawn_lifecycle: transient`, `harness_access: full`, `prompt: prompts/<id>.md`, `can_spawn: [...]`. Imports become `hya_bundle::{AgentRole, BundleCatalog, BundleSource, SourceFile}`.

**Why correct.** Instrumentation printed, for `nested_spawn_reaches_root_tree`:
```
ADVDIAG: uses_durable_admission_owner caller=build members=1 bg=true
ADVDIAG: prepare_spawn_admission entered caller=build
ADVDIAG: B runtime_fingerprint None
```
i.e. the request *does* take the durable-admission path and the return is specifically `runtime.rs:2011-2013`'s `.ok_or(SpawnError::Unavailable)?` on `runtime_semantic_fingerprint_v1` — not one of the other ~15 `Unavailable` sites. That chains to `TurnBinding::semantic_fingerprint_v1` (`crates/hya-core/src/runtime_registry.rs:553`), whose first line requires `catalog.semantic_identity_v1()`, which `BundleCatalog::from_prepared` **hard-sets to `None`** (`crates/hya-bundle/src/catalog.rs:42-52`). That is asserted product behavior (`crates/hya-bundle/tests/catalog.rs:166`), and production never uses `from_prepared` for its live catalog (`runtime.rs:66`, `:84` use `from_verified_catalogs`; `installed_bundle_refresh.rs:91` uses `with_verified_catalogs`; the only two non-test `from_prepared` calls, `hya-store/src/bundle_registry.rs:194,288`, discard their result).

**Verify (after Fix 3 — this alone still fails):** `cargo test -p hya-app --test nested_spawn_tree`

**Skip the optional tidy-up** (deleting `verified_admission_test_runtime` from `spawn_admission.rs` and calling `support::test_runtime`). It entangles clusters 1 and 2 while both are in flight. Take the minimum diff.

---

## Fix 3 — `nested_spawn_tree` fixture, part B: provider identity (test-only)

**File:** `/chivier-disk/yanweiye/Projects/yaca/crates/hya-app/tests/nested_spawn_tree.rs:34-35`

Add a local `IdentityFakeProvider` wrapper (mirroring `CountingProvider` at `spawn_admission.rs:122-163`) that delegates `id`/`capabilities`/`stream` to an inner `FakeProvider` and overrides only:
```rust
fn configured_identity_v1(&self) -> Option<Vec<u8>> { Some(b"hya-test-nested-spawn-identity-v1".to_vec()) }
```
then build the router as `ProviderRouter::new().with(Arc::new(IdentityFakeProvider { inner: FakeProvider::scripted(Vec::new()) }))`.

**Why correct.** Applying Fix 2 alone moved the failure forward to `capture err ProviderIdentityUnavailable` — an executed result, which is what proves this is a *second, independent* fixture gap rather than speculation. `AdmissionResolutionContext::capture` (`runtime.rs:2015-2016` → `canonical_provider_resolution`) needs `ProviderRouter::configured_identities_v1()`, which returns `None` if **any** provider leaves the trait default `configured_identity_v1 → None` (`crates/hya-provider/src/lib.rs:347`, doc-commented "Providers without a complete identity fail closed"). `FakeProvider` is the only thing in the tree that leaves the default; both real providers override it (`http.rs:464`, `dev.rs:71`). The identity bytes only need to be non-empty and stable — they feed `admission_binding_fingerprint_v1`, which these tests do not assert on.

**⛔ Do NOT add `configured_identity_v1` to `FakeProvider` itself.** That is the tempting one-line dedupe and it is wrong twice over: `crates/hya-app/src/runtime.rs:5016-5026` explicitly asserts a bare-`FakeProvider` router must fail closed with `ProviderIdentityUnavailable` (you'd trade 3 failures for 1), and `FakeProvider` is used across many crates, so an identity would perturb admission fingerprints far beyond that.

**Verify:** `cargo test -p hya-app --test nested_spawn_tree` → expect `3 passed`.

**Not papering over anything:** no assertion is weakened. The tree/roster/depth assertions are untouched and now genuinely execute through the durable-admission path for the first time.

**The p09 asymmetry confirms this is fixture drift, not regression:** `crates/hya-e2e/tests/p09_nested_subagent.rs` exercises the same durable-admission code against a real backend and passes, because production satisfies both preconditions. Pre-merge main (156d0ad3) had no `tests/support/mod.rs` at all — `nested_spawn_tree.rs` built its runtime inline. c231737d migrated it onto the unverified helper; adf46f9a later made the fingerprint mandatory; 994ea6b2 added a verified helper for `spawn_admission.rs` but never backfilled `support/mod.rs`.

---

## Fix 4 — `spawn_admission` debit test: bounded poll (test-only)

**File:** `/chivier-disk/yanweiye/Projects/yaca/crates/hya-app/tests/spawn_admission.rs`, lines 598-605

Replace the bare `assert_eq!(...remaining_budget(fixture.parent), 1)` with a `tokio::time::timeout(Duration::from_secs(5), ...)` loop yielding until the budget reaches 1 — same style as the two polls immediately above it (I read them; the journal-finalization poll at 1640-1656 is exactly this shape).

**Why correct.** The two observables are written by **different tasks** and are ordered against the test's favour: the *member* task finalizes the journal row to `Completed + logical_released` at `runtime.rs:1777` via `store().finalize_admission_members(...)` — bypassing hya-core's governor-releasing `finalize_spawn_admission` — before it even messages the owner; the *owner* releases the in-memory governor debit much later at `runtime.rs:3044` (`release_transient_operation`), after draining `completion_rx`, quiescing handles, and projecting the evidence envelope. The test passed when written because the background owner replied only at the end of `run()`; commit 3024b449 (`DurableOwnerReplyMode::BackgroundRunningOnRegister`) made background reply at registration and detach, destroying that implicit sync. The sibling `foreground_completion_uses_one_debit_and_one_finalize` (line 773) makes the identical bare assertion and still passes, because `ForegroundWholeBatch` still replies after release — that contrast is the proof.

**Do NOT "fix" the product by releasing at member-finalize time.** The operation debit is `cardinality` units released as one unit by the owner; per-member early release would be wrong for multi-member batches.

**Verify:** `cargo test -p hya-app --test spawn_admission -- --test-threads=1`

**⚠ Assertion-change flag (justified, with two honest caveats).**
1. The test's real claim survives intact: still bounded (5 s), still followed by the `retry` spawn that must be admitted under `per_run_budget = 1`.
2. **It tolerates a real, small product window**: between journal `Completed + logical_released` and the owner returning governor units, a concurrent spawn on the same root can be rejected `Overloaded`. That is a genuine design question worth filing separately — this test should not gate it, but do not let the fix erase the observation.
3. **The test name overpromises regardless of this fix**: `SubagentGovernor::release_operation` clamps with `.min(self.limits.per_run_budget)` (`crates/hya-core/src/orchestrator.rs:247`), so at `per_run_budget = 1` a double release is indistinguishable from a single one via `remaining_budget`. The poll is no weaker than the assert it replaces, but neither proves "its **exact** debit". Not a blocker.
4. This test can **flake green**: ~1 run in 40 observed the budget already restored at the assertion point. Do not treat a single green run at HEAD as evidence the fix is unnecessary.

---

## Fix 5 — `spawn_admission` queued-spawn: supervisor abort-on-closed-intake (**the only product edit — do last**)

**File:** `/chivier-disk/yanweiye/Projects/yaca/crates/hya-app/src/runtime.rs`, line 3410, in `spawn_team_supervisor_with_environment`

```rust
 let Some(bound_request) = bound_request else {
-    foreground_handlers.abort_all();
+    // Abort only on explicit shutdown; a closed intake (last sender dropped)
+    // must still drain already-admitted handlers to completion.
+    if stop_child.is_cancelled() {
+        foreground_handlers.abort_all();
+    }
     while let Some(joined) = foreground_handlers.join_next().await {
         observe_foreground_handler_join(Some(joined));
     }
     break;
 };
```

**Why correct.** The `else` branch is reached for two distinct reasons — stop-token cancelled, and intake closed because the last `BoundSpawnSender` dropped. Commit 2b6269d6 added the unconditional `abort_all()` for the *shutdown* case (its own doc: "Explicit shutdown cancels intake, aborts handlers, and drains the supervisor JoinSet") and inadvertently applied it to the closed-intake case, which previously drained. The test hands the supervisor one request and drops the sender (`spawn_admission.rs:1660 drop(forward_tx);`, which I read); `rx.recv()` returns `None` immediately, the in-flight durable-admission handler is aborted, its reply oneshot is dropped unused, and `hya-tool/src/spawn.rs:233` maps the closed oneshot to `SpawnError::Unavailable`. The chain is closed by reading, not correlation: `spawn_team_supervisor` `std::mem::forget`s the lifecycle (`runtime.rs:3331-3337`, verified) and a tokio-util `CancellationToken` does not cancel on drop, so `stop_child` is never cancelled in this test → the abort at 3367 is unreachable → 3410 is the **only** abort source, matching the observed `hya: foreground spawn handler failed (task 86 was cancelled)`.

Two executed controls, in both directions:
- test-side: changing only `drop(forward_tx)` → keepalive, against **unmodified** rlibs → test passes;
- product-side: applying only this guard, re-linking the **verbatim** repo test file → `25 passed; 1 failed` (only the Fix-4 debit test red), and `153 passed; 0 failed` on the hya-app lib.

The shutdown path is unchanged (both routes into this branch with the token set still abort), so `BuiltSessionEngine::shutdown()`/drop semantics are preserved.

**Verify:** `cargo test -p hya-app --test spawn_admission -- --test-threads=1` (expect 26 passed with Fix 4) and `cargo test -p hya-app --lib`.

### Honest caveat you must carry into review — this is *not* a live production bug

I verified the reviewer's counter-claim myself, and it holds: `SessionEngine` owns the `BoundSpawnSender` (`crates/hya-core/src/engine.rs:178-190` — `BoundSpawnSender { tx: mpsc::Sender<BoundSpawnRequest> }` — installed via `with_spawn_sender`, `runtime.rs:3976`), and the supervisor task holds `engine.clone()` (`runtime.rs:4121`). **While the supervisor task is alive, the sender is alive, so `rx.recv()` can never return `None` in production.** The closed-intake branch is reachable only from the test helper `spawn_team_supervisor`. So:

- The dramatic framing ("an admitted spawn is killed mid-flight, leaking a governor debit and leaving non-terminal `admission_journal` rows in production") is **unsupported**. This is a latent/dead-branch correctness fix.
- Consequently, **both fixes are defensible** and this is a judgement call, not a fact. The one-line test-side alternative (`spawn_admission.rs:1660`: `drop(forward_tx);` → `let _keepalive = forward_tx;`) is equally green.
- **I recommend the product-side guard** because (a) 2b6269d6's own design note says the abort was scoped to explicit shutdown, so this restores intended semantics rather than inventing them; (b) it preserves the fixture's coverage of "supervisor started after the request was already queued" instead of altering the scenario; (c) it was verified to break nothing. If your team decides "last spawn sender dropped" is *definitionally* shutdown, take the test-side line instead and record that decision — but do not take both.
- **Residual, unflagged by the original diagnosis:** the drain loop does not watch `stop_child`, so if intake ever did close with handlers in flight, a subsequent `shutdown()` would block until they finish rather than aborting — and `fail_after_claim` can await `std::future::pending::<()>()` (`runtime.rs:2842`). Unreachable in production for the ownership reason above; low severity; state it in the commit message. Do **not** expand the fix to make the drain loop stop-aware — that is scope creep on a dead branch.

---

## Ordering rationale

The clusters are independent, so the order is chosen for **attribution**, not dependency:

1. **Fix 1** (bundle_cli) — different crate, zero coupling, get it off the board.
2. **Fixes 2+3** (nested_spawn_tree) — must land **together**; Fix 2 alone converts the failure from `Unavailable`-at-fingerprint to `ProviderIdentityUnavailable` and the tests stay red.
3. **Fix 4** (debit poll) — test-only, isolates cleanly.
4. **Fix 5** (product) — **last**, so the full-workspace run after it attributes any new breakage unambiguously to the single product edit.

Commit 1-4 separately from 5 for the same reason.

---

## Regression surface — what to re-run beyond the direct tests

Blast radii, all checked: `grep -rn "support::" crates/hya-app/tests/` returns exactly one hit (`nested_spawn_tree.rs:42`), so Fix 2 has no other consumer (`spawn_admission.rs` declares `mod support;` but never calls it). Fixes 1, 3, 4 touch nothing outside their own test file. Fix 5 is the only one with real reach.

**Required after Fix 5** (everything that constructs a real `BuiltSessionEngine`, which the diagnosing agents did **not** run):
```
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
```
expecting **762 passed, 0 failed**. Pay specific attention to:
- `cargo test -p hya-app --lib` — includes `foreground_handler_cap_256` and the resident/admission unit suites (153 tests).
- `cargo test -p hya-server` and `cargo test -p hya-backend` — engine-lifecycle/shutdown paths, the ones no one has re-run against the patched supervisor.
- `cargo test -p hya-core` — `release_operation` has **eight** callers in `crates/hya-core/src/engine/admission.rs` (lines 105, 111, 141, 176, 199, 215, 249), not one as originally claimed; the recovery/claim paths there are the nearest neighbours to Fix 4's subject matter.

**Then** `cargo fmt --check` (exits 0 at HEAD — any diff is yours) and `cargo clippy --workspace --all-targets`.

**Two known flakes — do not misread them as new regressions:**
- `bundle_cli` shares `std::process::id()` across its 7 tests; `bundle_info_lists_prepared_static_resources` hit `AlreadyExists` on `fs::create_dir` (`bundle_cli.rs:406`) once in 24 whole-file runs. Pre-existing, out of scope.
- The Fix-4 debit test can pass without the poll roughly 1 run in 40.

**Watch item, caused by nothing here but adjacent:** `crates/hya-core/tests/support/mod.rs:59,130`, `crates/hya-core/src/test_support.rs:66`, `crates/hya-server/tests/support/mod.rs:111`, `crates/hya-app/tests/spawn_admission.rs:1629,2493,2751` still build catalogs with the unverified `BundleCatalog::from_prepared`. They pass today only because they never reach `prepare_spawn_admission`. Any future test that routes a spawn through durable admission from one of those fixtures will hit the identical wall as Fix 2. `spawn_admission.rs:1629` is the closest to the edge — it publishes a replacement catalog unverified, and only survives because the spawn under test is pinned to the older verified binding.

---

## Nothing is being deferred

All six failures have proven root causes and validated fixes; no cluster is being left unfixed for lack of evidence. The single open item is the **policy question in Fix 5** (is a closed intake a shutdown?), which needs a human decision, not more investigation — and either answer produces a green suite.