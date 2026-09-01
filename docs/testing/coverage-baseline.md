# Coverage baseline

First code-level coverage measurement of this workspace. Before this, "coverage"
meant a scenario inventory (`crates/hya-e2e/matrix.toml`); there was no line
data at all.

| | |
| --- | --- |
| Date | 2026-08-05 |
| Commit | `1a7db256` |
| Tool | `cargo-llvm-cov 0.8.7` (LLVM source-based instrumentation) |
| Toolchain | `rustc 1.91.1 (ed61e7d7e 2025-11-07)` |

## Regenerate

```sh
cargo install cargo-llvm-cov --locked

# Collect, then report separately — a failing test must not cost you the data.
cargo llvm-cov --no-report --workspace --exclude hya-e2e --no-fail-fast
cargo llvm-cov report --summary-only
```

Do **not** combine `--ignore-run-fail` with `--no-fail-fast`; `cargo-llvm-cov`
rejects that pair. The two-step form above is the reliable way to get a report
even when a test fails.

## Workspace totals (Track I + unit tests; `hya-e2e` excluded)

| Metric | Total | Missed | Covered |
| --- | ---: | ---: | ---: |
| Lines | 63,386 | 9,155 | **85.56%** |
| Regions | 90,758 | 14,654 | 83.85% |
| Functions | 6,405 | 1,267 | 80.22% |

## Per crate, by line coverage

| Coverage | Lines | Missed | Crate |
| ---: | ---: | ---: | --- |
| 0.0% | 1 | 1 | hya-plugin-example |
| 0.0% | 60 | 60 | hya-client |
| 51.8% | 1,461 | 704 | hya-backend |
| 61.7% | 940 | 360 | hya-updater |
| 64.3% | 709 | 253 | xtask |
| 67.9% | 2,195 | 705 | hya-sdk |
| 76.9% | 13 | 3 | hya |
| 79.6% | 530 | 108 | hya-ts |
| 82.3% | 1,986 | 351 | hya-bundle |
| 83.9% | 1,615 | 260 | hya-plugin |
| 84.8% | 191 | 29 | hya-native |
| 85.3% | 2,672 | 393 | hya-store |
| 87.2% | 18,169 | 2,324 | hya-app |
| 87.2% | 10,908 | 1,391 | hya-core |
| 88.5% | 2,618 | 302 | hya-provider |
| 88.7% | 5,111 | 577 | hya-tool |
| 89.3% | 952 | 102 | hya-mcp |
| 90.4% | 12,049 | 1,162 | hya-server |
| 94.2% | 1,206 | 70 | hya-proto |
| **85.6%** | **63,386** | **9,155** | **all** |

### Reading these numbers honestly

- **`hya-client` at 0% is an artifact of this measurement, not a real gap.**
  It is consumed by `crates/hya-e2e`, which this run excludes. It is exercised
  in reality — by Track P, whose contribution this run cannot see. The Track P
  measurement below puts it at **98.3%**, which settles it.
- **`hya-backend` at 51.8%** is the lowest genuine figure. It is the CLI/binary
  crate, where argument parsing and process wiring are exercised by running the
  real binary — again, mostly Track P territory.
- One test target (`-p hya-app --lib`) failed during collection. It is the
  known load-dependent flake `recovered_promotions_reconstruct_each_parent_binding`
  (see
  `.trellis/tasks/archive/2026-08/08-05-land-swarm-branch-to-main/findings.md`).
  Its effect on the totals is negligible, but the numbers above come from a
  run with one red target, not a fully green one.
- These are **not** a quality target. This is a baseline. Nothing in this task
  adds tests to move it.

## Track P's contribution — measured

At the 2026-08-06 measurement, Track P (`crates/hya-e2e`: 18 binaries,
27 scenarios) was excluded from the workspace run above and measured
separately. It was the measured suite that drove the **real** `hya-backend`
binary over HTTP and covered that process/serving path end to end. These counts
describe commit `3f18e6e5` plus the noted change, not the current matrix.

| | |
| --- | --- |
| Date | 2026-08-06 |
| Commit | `3f18e6e5` (+ the graceful-shutdown change) |
| Tool | `cargo-llvm-cov 0.8.7` |
| Toolchain | `rustc 1.91.1 (ed61e7d7e 2025-11-07)` |

### Why this needed a code change first

The earlier attempt produced `hya-server` and `hya-core` at **0.0%** — impossible
for a suite that serves 25 real sessions. Root cause: the harness stopped each
backend with `std::process::Child::kill()`, which is **SIGKILL** on Unix. LLVM
writes `.profraw` from an atexit handler, and a SIGKILL'd process never runs one.

Sending SIGTERM instead was necessary but *not* sufficient: `hya-backend serve`
installed no signal handler, and the default SIGTERM disposition also skips
atexit handlers. Two changes were needed, and both are now in place:

- `crates/hya-backend/src/serve.rs` installs SIGTERM/SIGINT/SIGHUP handlers
  **before** printing the listen line and passes them to
  `axum::serve(...).with_graceful_shutdown(...)`. Previously `axum::serve` never
  returned, which also made the existing `built.shutdown()` teardown unreachable.
- `crates/hya-e2e/src/backend.rs` spawns the backend as its own process-group
  leader and stops it with SIGTERM → bounded poll for exit (≤1s, returns as soon
  as the child is reaped) → **unconditional** SIGKILL of the group.

The discriminator is the exit status, not "it stopped": a backend that returned
from `main` reports `status.code() == Some(0)`; one killed by a signal reports
`None`. Measured against the pre-change binary the harness sees `None`; against
the current one, `Some(0)` in ~15 ms. That assertion is now a regression test,
`backend::tests::shutdown_stops_the_backend_cleanly_with_exit_status_zero`.

### Regenerate

`HYA_E2E_BACKEND_BIN` points the harness at any build, so a coverage run no
longer has to overwrite `target/debug/hya-backend` (which breaks concurrent work).

```sh
# Instrumented build goes to its own target dir; target/debug is left alone.
export CARGO_TARGET_DIR=/path/to/cov-target
cargo llvm-cov clean --workspace
eval "$(cargo llvm-cov show-env --sh)"
cargo build --bin hya-backend
HYA_E2E_BACKEND_BIN="$CARGO_TARGET_DIR/debug/hya-backend" \
  cargo test -p hya-e2e -- --test-threads=1
cargo llvm-cov report --summary-only
```

### Result

All 18 Track P binaries passed (27 scenarios). The run produced **77** `.profraw`
files, against **6** before the change.

| Coverage | Lines | Missed | Crate |
| ---: | ---: | ---: | --- |
| 2.8% | 1,309 | 1,273 | hya-plugin |
| 21.6% | 11,792 | 9,248 | hya-server |
| 27.1% | 2,054 | 1,498 | hya-provider |
| 31.7% | 1,182 | 807 | hya-backend |
| 37.8% | 7,037 | 4,376 | hya-app |
| 47.6% | 4,636 | 2,431 | hya-tool |
| 49.0% | 2,511 | 1,281 | hya-store |
| 51.2% | 7,629 | 3,720 | hya-core |
| 53.4% | 470 | 219 | hya-mcp |
| 57.4% | 1,937 | 825 | hya-bundle |
| 72.5% | 756 | 208 | hya-proto |
| 84.3% | 1,302 | 204 | hya-e2e |
| 98.3% | 60 | 1 | hya-client |
| **38.9%** | **42,675** | **26,091** | **all (Track P only)** |

Totals: lines 38.86%, regions 36.71%, functions 35.18%.

### Reading these numbers honestly

- **The pass signal is `hya-server` and `hya-core` moving off 0.0%** — to 21.6%
  and 51.2%. Those two rows are the whole point; they are what a suite serving 25
  real sessions must touch, and they were previously reporting nothing at all.
- **`hya-client` 98.3% (was 0.0%)** closes the hole flagged in the workspace
  table above. `hya-client` is used only by this harness, so the workspace run's
  0.0% was always a measurement artifact, and this is the number that shows it.
- **The line denominator here (42,675) is smaller than the workspace one
  (63,386)** because only the crates reachable from `cargo test -p hya-e2e` get
  built and instrumented. Do not read 38.9% as "Track P covers 38.9% of the
  workspace" — the two percentages have different denominators and are not
  additive. Use the per-crate rows, not the total.
- **`hya-plugin` at 2.8%** was genuine for this measured commit: no recorded
  Track P scenario loaded a plugin. Current P18 coverage was added later; it
  does not change this historical row.

### Cost

Track P wall clock, uninstrumented, `--test-threads=1`, no rebuild in the timed
window (a run that also compiles reports ~7.5 s and measures the compiler):

| | Tests | Wall clock | Sum of per-binary times |
| --- | ---: | ---: | ---: |
| Before (SIGKILL) | 27 | 3.49 s | 3.24 s |
| After (SIGTERM + bounded poll) | 27 | 3.54 s | 3.30 s |
| After, incl. the new shutdown regression test | 30 | 3.41–3.69 s | 3.19 s |

About +50 ms across 25 backends, i.e. ~2 ms each — inside run-to-run noise. The
grace period is a **poll** (5 ms interval, 1 s ceiling) that returns the moment
the child is reaped, not a fixed sleep; a fixed 1 s sleep would have added ~25 s.

### No-orphan guarantee

The SIGKILL escalation is unconditional after the grace period, so a backend that
ignored SIGTERM would still be destroyed, and the group signal reaps the
backend's own children. Verified rather than reasoned about: `pgrep -af hya-e2e`
returns nothing both after a clean full run and after a run with a deliberately
panicking scenario (`Drop` runs during unwinding).


## Not done here

- No coverage gate or threshold in CI. That decision needs this baseline first.
- No upload to a coverage service.
- No tests written to raise the number.
