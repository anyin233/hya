# Implement — retire unverified catalog test fixtures

Test-only work plus one spec correction. Steps 2–5 are independent and may be
done in any order; step 1 is a no-op that must still be recorded.

## Step 1 — R2: record the no-op

- [ ] No call-site migrations. The survey found exactly **1** of 99 sites can
      reach `prepare_spawn_admission`, and it is the deliberately-unverified
      `spawn_admission.rs:1639`, which must stay unverified.
- [ ] Do **not** "migrate a few anyway for consistency". Every one of the other
      98 is unreachable; touching them is the churn the PRD forbids.

## Step 2 — R3: delete the duplicate helper (the load-bearing change)

- [ ] Delete `verified_admission_test_runtime` from
      `crates/hya-app/tests/spawn_admission.rs`.
- [ ] Rewire its **4 call sites** (fanning out to 11 tests) to
      `support::test_runtime`. `mod support;` is already declared at line 3 and
      currently unused — this is what makes it live.
- [ ] Confirm the two bodies really are identical before deleting (they were
      md5 `e527d528bd4df179e2ff83e25050134d`); if they have diverged since the
      survey, STOP and report rather than picking one.

**Validation**
```
cargo test -p hya-app --test spawn_admission
```
All 11 dependent tests must pass. Preserve the existing mitigation at
`spawn_admission.rs:1636` (`assert quick.prompt == "quick prompt"`) — it exists so
helper drift fails loudly.

## Step 3 — R4: move `IdentityFakeProvider`

- [ ] Move it from `crates/hya-app/tests/nested_spawn_tree.rs:36-67` into
      `crates/hya-app/tests/support/`, and use it from there.
- [ ] **Do NOT add `configured_identity_v1` to `FakeProvider`.** The assertion
      that a bare-`FakeProvider` router fails closed is at
      `crates/hya-app/src/runtime.rs:5021-5031` (the PRD's `5016-5026` is stale
      line drift — verify before quoting it anywhere).
- [ ] `FakeProvider` has 290 references across 103 files; none should change.
      If your diff touches any file outside `hya-app/tests/`, you have gone wrong.

**Validation**
```
cargo test -p hya-app --test nested_spawn_tree
cargo test -p hya-app --lib -- runtime::tests
```

## Step 4 — R5: fail loudly on ids that corrupt the generated YAML

The PRD asks for a guard against YAML 1.1 bareword booleans. **That premise did
not reproduce** — see `design.md`. Implement the *intent*, not the letter.

- [ ] First, re-confirm the hazard set against the **real** prepare path
      (`hya_bundle::prepare_builtins`), not against a standalone `serde_norway`
      probe. The survey's probe may under-report what hya-bundle rejects.
- [ ] Assert loudly in `support::test_runtime` on ids that would silently corrupt
      the string-concatenated `bundle.yaml`: `,` (splits `can_spawn`), a leading
      `&` (anchor → empty id), leading/trailing whitespace (trimmed), and `""`.
- [ ] Add a comment recording that the YAML-1.1 bareword list was tested and does
      **not** apply here, so the next reader does not "fix" it back.
- [ ] Do not assert against `no`/`on`/`y` — a guard that catches nothing is worse
      than none, because it is believed.

## Step 5 — R6: bound the unbounded wait

- [ ] `crates/hya-app/tests/spawn_admission.rs:1681` awaits `queued_spawn` (a
      `JoinHandle`) with two bare `expect`s. Wrap in
      `tokio::time::timeout(Duration::from_secs(5), …)`, matching the file's own
      idiom at `:981-985` and `:1169-1172` (20 of its 25 timeouts use 5s).
- [ ] The point is that a non-replying supervisor **fails** instead of hanging the
      whole test binary. Verify by construction, not by assumption: the timeout
      must be on the join, not on an inner future.

## Step 6 — spec correction

- [ ] `.trellis/spec/backend/task-tool.md:218-221` says durable admission applies
      to "background" spawns. That is too narrow: `uses_durable_admission_owner`
      returns `true` for foreground multi-member all-transient spawns as well
      (`if req.background { len() == 1 } else { true }`). Correct it — this task
      exists to stop the next person hitting this wall, and a wrong trigger in the
      spec is how they would.

## Step 7 — deferred, recorded explicitly

- [ ] The `harness_access` signature change: only **one** site would need it, and
      it would also need per-agent prompt overrides
      (`ROOT_MAIN_BUNDLE_PROMPT` / `NESTED_CALLER_BUNDLE_PROMPT`), touching 12
      call sites. Since R2 is a no-op there is no fixture that needs it.
      **Deferred, with this reason recorded** — not silently skipped.

## Step 8 — verification gate

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- matrix-check
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
bash scripts/verify-no-http.sh
```

Redirect each to a file and grep it; never pipe through `tail`. Expected suite
size ≈1324 tests / 237 binaries — a materially smaller number means the capture
is incomplete, not that the suite shrank.

## Step 9 — commit

Stage only the files in `design.md` § Blast radius. The tree carries unrelated
uncommitted work (`crates/hya-sdk/src/{reducer,store,types}.rs`,
`.trellis/tasks/07-23-remove-rust-tui/**`, the `fixtures/*` and `imgs/*`
deletions) belonging to other in-flight tasks — never stage it.

One-line semantic messages, atomic scope, no agent attribution. Version stays
`0.34.13`.

## Rollback

Every change is test-only plus one spec doc. Reverting any commit restores the
prior state with no migration and no product impact.
