# Implement — shut the e2e backend down gracefully

Ordered. Step 1 is the product change and must land before step 3 can show any
coverage movement — the harness change alone flushes nothing.

## Step 1 — product: `hya-backend serve` handles SIGTERM (R2)

- [ ] Add a `termination_signal()` helper in `crates/hya-backend/src/serve.rs`,
      mirroring `crates/hya-ts/src/main.rs:354-360`
      (`SignalKind::terminate()` selected against `ctrl_c()`).
- [ ] Attach it to the existing `axum::serve` call at `serve.rs:55` via
      `.with_graceful_shutdown(...)`.
- [ ] No manifest change needed — workspace `tokio` already has `features = ["full"]`.
- [ ] Do **not** restructure `cmd_serve`. The existing
      `built.shutdown().await` at `:57` is already correct; it is simply
      unreachable today. Making it reachable is the entire point.

**Validation — prove the exit path, do not assume it**
```
cargo build -p hya-backend --bin hya-backend
```
Then start `hya-backend serve` on an ephemeral port, send SIGTERM, and confirm:
- the process exits with **status code 0** (`status.code() == Some(0)`), not a
  signal death — this is the discriminator that atexit handlers ran;
- it exits promptly (well under 1s).

Record the observed exit status and elapsed time. "It stopped" is not evidence.

## Step 2 — harness: SIGTERM + bounded wait + SIGKILL (R1, R3)

- [ ] Add `libc = { workspace = true }` to `crates/hya-e2e/Cargo.toml`.
- [ ] Spawn the backend with `.process_group(0)` (`backend.rs`, near `:167-193`)
      so it leads its own process group.
- [ ] Replace `Drop`'s `self.child.kill()` (`:245-251`) with:
      `libc::kill(-pid, SIGTERM)` → poll for exit up to a bounded grace period →
      `libc::kill(-pid, SIGKILL)` → reap. Follow the shape at
      `crates/hya-sdk/src/server.rs:189-209`.
- [ ] **Bounded poll, not a fixed sleep.** Track P runs 25 scenarios in ~2s; a
      1s sleep per backend would dominate it. Return as soon as the child is
      reaped.
- [ ] The SIGKILL escalation must be **unconditional** after the grace period.
      That is what preserves the no-orphan guarantee (R3) for a backend that
      ignores SIGTERM. Do not make it conditional on a clean-exit check.
- [ ] There is a second `child.kill()` at `:211` on the startup-failure path —
      check whether it needs the same treatment, and say which you chose and why.

## Step 3 — R5: `HYA_E2E_BACKEND_BIN`

- [ ] `default_backend_bin()` (`backend.rs:353`) should honour
      `HYA_E2E_BACKEND_BIN` when set, falling back to today's
      `target/debug/hya-backend` resolution otherwise.
- [ ] This is what lets an instrumented build be measured without clobbering the
      normal binary.

## Step 4 — R3: prove no orphans, deliberately

- [ ] After a full Track P run: assert **zero** stray `hya-backend` processes
      (`pgrep -f hya-backend`). Capture the actual command output.
- [ ] Then **deliberately** make one scenario panic, run again, and re-check.
      The PRD asks for this explicitly — `Drop` runs during unwind, but verify it
      rather than reasoning about it. Revert the sabotage afterwards and
      re-confirm green.

## Step 5 — R4: measure Track P coverage

- [ ] Record Track P wall-clock **before** and **after** the change (PRD
      acceptance criterion). A material regression means the grace period was
      implemented as a sleep.
- [ ] Measure with `cargo llvm-cov` (installed at
      `/home/yanweiye/.cargo/bin/cargo-llvm-cov`). Use `HYA_E2E_BACKEND_BIN` from
      step 3 to point at the instrumented binary instead of overwriting
      `target/debug/hya-backend`.
- [ ] **The pass/fail signal is specific**: `hya-server` and `hya-core` must show
      **non-zero** line coverage. They were 0.0%, which is impossible for a suite
      serving 25 real sessions, and is the exact symptom of the missing flush.
      If they are still 0.0%, the change did not work — report that rather than
      publishing a nicer-looking number.
- [ ] Update `docs/testing/coverage-baseline.md`: replace the
      "Track P's contribution — measured, and why the number is not publishable"
      section with the real numbers, and note that the `hya-client` 0.0% is
      explained by it being harness-only.

## Step 6 — verification gate

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- matrix-check
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
bash scripts/verify-no-http.sh
```

Redirect each to a file and grep it; **never pipe through `tail`**. Current
baseline on this branch: **238 binaries, 1310 passed, 0 failed, 3 ignored**;
e2e **18 binaries, 27 passed**. A materially smaller number means an incomplete
capture, not a shrunken suite.

## Step 7 — commit

Stage only the files in `design.md` § Blast radius. The tree carries unrelated
uncommitted work (`crates/hya-sdk/src/{reducer,store,types}.rs`,
`.trellis/tasks/07-23-remove-rust-tui/**`, the `fixtures/*` and `imgs/*`
deletions) belonging to other in-flight tasks — never stage it.

One-line semantic messages, atomic scope, no agent attribution. **This one
touches product code**, so keep the product change in its own commit, separate
from the harness and docs.

Version stays `0.34.13` unless the reviewer decides the shutdown behaviour change
is user-visible enough to warrant a bump — flag it, do not decide it silently.
