# Shut the e2e backend down gracefully

Follow-up from the E2E hardening round. Evidence and the failed measurement are
recorded in `docs/testing/coverage-baseline.md` §"Track P's contribution".

## Goal

Make the Track P harness stop the backend process gracefully, so that the
backend's coverage can be measured at all — and so that whatever else depends on
a clean exit (flushes, teardown, temp cleanup) actually happens.

## Why this exists

`crates/hya-e2e/src/backend.rs`:

```rust
impl Drop for BackendProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        …
```

`std::process::Child::kill()` sends **SIGKILL** on Unix. A SIGKILL'd process
never runs atexit handlers, and LLVM writes `.profraw` from an atexit handler.

That was measured, not inferred. Running Track P with an instrumented backend
produced:

| Crate | Line coverage |
| ---: | --- |
| hya-backend | 23.9% |
| hya-store | 8.8% |
| **hya-server** | **0.0%** |
| **hya-core** | **0.0%** |

`hya-server` and `hya-core` at 0.0% is impossible for a suite that serves 25
real sessions — those numbers come only from the short-lived
`hya-backend bundle …` CLI invocations in `p11_hyabundle.rs`, which exit
normally. Every `serve` process was destroyed before it could flush.

Consequence today: the workspace coverage baseline (85.56% lines) **excludes
Track P entirely**, and `hya-client` shows a misleading 0.0% because it is used
only by the harness.

## Requirements

- R1. Stop the backend with SIGTERM and wait for normal exit, escalating to
  SIGKILL only after a bounded grace period. `crates/hya-sdk/src/server.rs`
  already implements exactly this shape (SIGTERM to the **process group**, ~1s
  grace, then SIGKILL) — reuse that approach rather than inventing one.
- R2. Verify the backend actually handles SIGTERM by exiting cleanly. If
  `hya-backend serve` does not install a signal handler, graceful shutdown is
  not achievable by the harness alone and that becomes the finding — say so
  rather than adding a longer sleep and calling it done.
- R3. The harness must still guarantee no orphaned backend processes, including
  when a test panics. That guarantee is why `kill()` is there; do not trade it
  away for a graceful path that leaks on the failure branch.
- R4. Once R1–R3 hold, measure Track P's coverage contribution and update
  `docs/testing/coverage-baseline.md`, replacing the "unmeasurable" section with
  real numbers.
- R5. Consider an env override for the backend binary path (e.g.
  `HYA_E2E_BACKEND_BIN`) so coverage runs can point the harness at an
  instrumented build without overwriting `target/debug/hya-backend`. The
  measurement recipe currently requires clobbering that path, which is
  workable but hostile to concurrent work.

## Constraints

- Track P currently passes 25 scenarios in ~2s total. A 1s grace period per
  backend would multiply that. Measure the cost; if it is material, the grace
  period should be "wait for exit, up to N ms" rather than a fixed sleep.
- Do not regress the no-orphan guarantee. A leaked `serve` process holds a port
  and a SQLite file and will make later scenarios fail confusingly.
- `p11_hyabundle.rs` already exercises clean-exit CLI paths; they are the
  control group for "coverage data does get written when a process exits
  normally".

## Acceptance criteria

- [ ] The harness terminates backends via SIGTERM + bounded wait + SIGKILL
      fallback.
- [ ] No orphaned `hya-backend` processes after a full Track P run, including a
      run where a scenario panics (verify deliberately).
- [ ] Track P wall-clock cost recorded before and after.
- [ ] Track P coverage measured, with `hya-server` and `hya-core` showing
      non-zero line coverage — the specific signal that the flush now happens.
- [ ] `docs/testing/coverage-baseline.md` updated with the real Track P numbers
      and the "unmeasurable" section removed.
- [ ] `cargo test -p hya-e2e -- --test-threads=1` still green.

## Out of scope

- Adding a coverage gate or threshold to CI.
- Raising coverage.
