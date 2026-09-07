# Implement — fix the four known flaky tests

Ordered checklist. Steps 1–2 are independent of the item-1 reproduction and can
proceed while it runs.

## Step 1 — `bundle_cli` unique data root (item 3)

- [ ] Add to `crates/hya-backend/tests/bundle_cli.rs`:
      `fn unique_data_root() -> Result<PathBuf, …>` combining `process::id()`,
      `as_nanos()`, and a `static AtomicU64` serial — mirroring the existing
      idiom at `crates/hya-app/src/runtime.rs:10029`.
- [ ] Replace all 7 inline `let nanos = …; let data_root = …; fs::create_dir(…)`
      blocks (lines 29, 125, 208, 298, 345, 401, 487) with a call to it.
- [ ] The helper creates the directory; callers must not re-create it.

**Validation**

```sh
cargo test -p hya-backend --test bundle_cli
```

**Mutation gate (required — a green test here proves little on its own).**
Temporarily drop the serial from the helper, then run the 7-thread collision
probe shape against the *helper itself* (7 threads calling `unique_data_root()`
simultaneously, asserting all paths distinct, 200k rounds). It must report
collisions without the serial and zero with it. Record both numbers.
Without this, "40/40 green" is indistinguishable from the pre-fix state, which
was also 40/40 green.

## Step 2 — `frontend_cli` ETXTBSY (item 2)

- [ ] Change `temp_dir()` (line 306) to root under `env!("CARGO_TARGET_TMPDIR")`
      instead of `std::env::temp_dir()`, and add the atomic serial.
- [ ] In `missing_adjacent_launcher_reports_its_path` (line 119), replace
      `fs::write(&relocated, fs::read(CARGO_BIN_EXE_hya)?)?` +
      `set_permissions` with `std::fs::hard_link(env!("CARGO_BIN_EXE_hya"), &relocated)?`.
      The mode is inherited from the linked inode, so `set_permissions` goes away
      — do not re-add it, it would mutate the real binary's inode.
- [ ] Confirm the assertion at line 131 still passes: it requires
      `current_exe()` to report the *link's* path.

**Validation**

```sh
cargo test -p hya --test frontend_cli
```

**Mutation gate.** Revert only the `hard_link` line to the old `fs::write` form
and run the etxtbsy probe shape again; the ETXTBSY window must be present before
and absent after. The 18.5 % probe rate is the before-number; the after-number
must be 0 over an equal number of attempts.

**Watch for**: if `hard_link` fails `EXDEV` on a machine where
`CARGO_TARGET_TMPDIR` and the binary somehow differ, the test must fail loudly
with that error rather than silently falling back to a copy. A silent fallback
would reintroduce the race on exactly the machines where it matters.

## Step 3 — item 1, gated on the reproduction result

- [ ] Read `t1_repro.log`. Report failures / total, and state the load condition.
- [ ] **If reproduced**: capture the actual failing values, confirm which of the
      two candidate paths in `design.md` fires (record-equality `continue` vs
      empty promotion), fix the mechanism, and prove it by mutation.
- [ ] **If not reproduced**: write the "not established" record — what was ruled
      out (temp-path collision via `NEXT_TEMP_ID`; the `16bde844` guard), what
      the leading candidates are, and what evidence would settle it. Then take
      the PRD R4 decision explicitly.
- [ ] Either way: state the run count. A green loop is not a fix.

**Do not** widen the `continue` at `runtime.rs:2260` into a hard error as a
speculative fix — that changes production recovery behaviour on an unproven
hypothesis.

## Step 4 — item 4 documentation

- [ ] In `docs/testing/agent-matrix.md`, record that `pty-smoke.test.ts` runs
      non-gating (`continue-on-error: true`, `fee38938`), with the observed
      failure rate and the reason it is not chased further.

## Step 5 — full verification gate

Run in order, each redirected to a file (never piped through `tail` — a
truncated capture already produced one wrong test count in the previous round):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- matrix-check
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
bash scripts/verify-no-http.sh
```

Expected suite size ≈1324 tests / 237 binaries. **A materially smaller number
means the capture is incomplete, not that the suite shrank** — verify before
reporting.

## Step 6 — commit

Stage only the files listed in `design.md` § Blast radius. The working tree
carries unrelated uncommitted work (`crates/hya-sdk/src/{reducer,store,types}.rs`,
`.trellis/tasks/07-23-remove-rust-tui/**`, the `fixtures/*` and `imgs/*`
deletions) that belongs to other in-flight tasks and must not be swept in.

One-line semantic message, no agent attribution. Version stays `0.34.13`.

## Rollback

Every change is test-only (plus one doc file) unless item 1 turns out to be a
product defect. Reverting any single commit restores the prior state with no
migration and no product impact.
