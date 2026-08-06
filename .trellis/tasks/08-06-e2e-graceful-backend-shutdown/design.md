# Design — shut the e2e backend down gracefully

## The finding that reshapes this task (PRD R2)

The PRD asks the harness to send SIGTERM instead of SIGKILL, and adds R2: *verify
the backend actually handles SIGTERM; if it does not, say so rather than adding a
longer sleep.*

**It does not.** Verified by search: the only signal handling in the workspace is
`crates/hya-ts/src/main.rs:354-360`. Neither `hya-backend` nor `hya-server`
installs one, and neither declares a signal dependency.

This matters more than it first appears. **The default disposition for SIGTERM
also terminates the process without running atexit handlers**, exactly like
SIGKILL. So changing the harness alone — R1 in isolation — would produce a
tidier-looking kill that still flushes **no** `.profraw`, and Track P coverage
would stay at 0.0% for `hya-server` and `hya-core`. The harness change is
necessary but not sufficient; a product-side handler is required.

R2 anticipated this exact outcome. Recording it as the finding, and then fixing
it, is the honest reading — not stopping at "the harness now sends SIGTERM".

## The second finding: the graceful path already exists and is unreachable

`crates/hya-backend/src/serve.rs:55-59`:

```rust
let serve_result = axum::serve(listener, server_router(state)).await.context("serve http");
let shutdown_result = built.shutdown().await.context("shutdown spawn supervisor");
serve_result.and(shutdown_result)
```

`axum::serve(...)` without `.with_graceful_shutdown(...)` runs until the process
dies. So `built.shutdown()` — which drains the spawn supervisor — is **dead code
in practice today**: nothing ever reaches it. The clean-exit machinery was
written; it was just never given a way to trigger.

That makes the product change small and low-risk: give `axum::serve` a shutdown
future, and the already-written teardown starts running.

## Design

### A. Product — `hya-backend serve` handles SIGTERM

- Add a `termination_signal()` helper mirroring the existing idiom at
  `crates/hya-ts/src/main.rs:354-360` (`SignalKind::terminate()` selected against
  `ctrl_c()`). `tokio` is workspace-wide with `features = ["full"]`, so
  `tokio::signal` needs no manifest change.
- Pass it to `.with_graceful_shutdown(...)` on the existing `axum::serve` call.

`cmd_serve` then returns `Ok(())` normally, `main` returns, the process exits via
`exit()`, atexit handlers run, and LLVM writes `.profraw`. This is the whole
mechanism the coverage measurement needs.

### B. Harness — `crates/hya-e2e/src/backend.rs`

Replace `Drop`'s `self.child.kill()` (SIGKILL) with the shape already proven in
`crates/hya-sdk/src/server.rs:189-209`:

1. Spawn the child with `.process_group(0)` so it leads its own group. Without
   this, a group signal would hit the test runner itself, and a single-pid kill
   would orphan any grandchildren.
2. On drop: `libc::kill(-pid, SIGTERM)`, then wait for exit up to a bounded
   grace period, then `libc::kill(-pid, SIGKILL)` and reap.

`libc` is a workspace dependency; `hya-e2e` adds `libc = { workspace = true }`.

**R3 — the no-orphan guarantee must not be traded away.** `Drop` runs during
panic unwinding, so a failing scenario still reaches this path. The SIGKILL
fallback is unconditional after the grace period, so a backend that ignores
SIGTERM is still destroyed. The guarantee is preserved *because* the escalation
is not optional.

**Grace period — PRD constraint.** Track P currently runs 25 scenarios in ~2s
total. A fixed 1s sleep per backend would dominate that. So the wait must be
"poll for exit, up to N ms", returning as soon as the child is reaped — a healthy
backend exits in milliseconds and costs nothing measurable. The PRD explicitly
asks for this shape rather than a fixed sleep.

### C. R5 — `HYA_E2E_BACKEND_BIN` override

`default_backend_bin()` (`backend.rs:353`) hardcodes `target/debug/hya-backend`,
so the documented coverage recipe requires overwriting that path — hostile to
concurrent work. Honour `HYA_E2E_BACKEND_BIN` when set, falling back to the
current behaviour. This is what lets an instrumented build be measured without
clobbering the normal binary.

## Verification plan (what makes this real rather than plausible)

The failure mode to avoid is a change that *looks* graceful while still flushing
nothing. Two checks, both falsifiable:

1. **Exit path.** Confirm the backend exits **0** on SIGTERM and that
   `built.shutdown()` actually runs. A backend killed by signal reports a signal
   exit, not status 0 — so `status.code() == Some(0)` is the discriminator.
2. **The coverage signal itself (R4).** `hya-server` and `hya-core` moving off
   **0.0%** is the specific evidence that the flush now happens. If they stay at
   0.0%, the change did not work regardless of how clean the code looks.

**No-orphan check (R3):** after a full Track P run, and separately after a run
with a deliberately panicking scenario, assert no `hya-backend` processes remain.
Verify deliberately, per the PRD — do not assume `Drop` covered it.

**Cost check:** record Track P wall-clock before and after. If the grace period is
implemented as a bounded poll rather than a sleep, the delta should be
negligible; a material delta means it was implemented as a sleep.

## Blast radius

| File | Change |
| --- | --- |
| `crates/hya-backend/src/serve.rs` | `termination_signal()` + `.with_graceful_shutdown(...)` — **product** |
| `crates/hya-e2e/src/backend.rs` | process group, SIGTERM→wait→SIGKILL, `HYA_E2E_BACKEND_BIN` |
| `crates/hya-e2e/Cargo.toml` | `libc = { workspace = true }` |
| `docs/testing/coverage-baseline.md` | replace the "unmeasurable" section with real numbers |

The product change affects `hya-backend serve` shutdown only. It cannot change
request handling: `with_graceful_shutdown` alters when the accept loop stops, not
how requests are served.

## Rollback

The harness and doc changes are test-only. The product change is a single call
added to `axum::serve` plus a helper; reverting it restores the previous
never-returns behaviour with no migration and no persisted state.
