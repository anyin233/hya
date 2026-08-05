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
  in reality — by Track P, whose contribution this run cannot see. That single
  row is the clearest argument for measuring Track P separately.
- **`hya-backend` at 51.8%** is the lowest genuine figure. It is the CLI/binary
  crate, where argument parsing and process wiring are exercised by running the
  real binary — again, mostly Track P territory.
- One test target (`-p hya-app --lib`) failed during collection. It is the
  known load-dependent flake `recovered_promotions_reconstruct_each_parent_binding`
  (see `.trellis/tasks/08-05-land-swarm-branch-to-main/findings.md`). Its effect
  on the totals is negligible, but the numbers above come from a run with one
  red target, not a fully green one.
- These are **not** a quality target. This is a baseline. Nothing in this task
  adds tests to move it.

## Track P's contribution — measurement blocked, and why

`cargo llvm-cov` builds instrumented binaries into `target/llvm-cov-target/debug/`,
but the E2E harness spawns the backend from a hard-coded
`target/debug/hya-backend` (`default_backend_bin()` in
`crates/hya-e2e/src/backend.rs`). Running Track P under `cargo llvm-cov` as-is
would therefore execute an **uninstrumented** backend: the child process emits
no profile data, and the report would show Track P contributing almost nothing
— silently, with no error.

That undercount is exactly the failure mode this document must not publish.

A workable approach that needs no source change: build the backend under
instrumentation, place it where the harness looks, then run Track P with
`--no-report` and generate the report afterwards. The harness sets many env vars
on the child but does not clear `LLVM_PROFILE_FILE`, so an instrumented child
does emit profraw data.

Status: **not yet measured.** It requires exclusive use of
`target/debug/hya-backend`, which was in use by concurrent work when this
baseline was taken. Recorded as the next step rather than guessed at.

A cleaner long-term fix is an env override in `default_backend_bin()` (e.g.
`HYA_E2E_BACKEND_BIN`) so the harness can be pointed at any build without
overwriting artifacts. That is a harness change, out of scope for a measurement
task.

## Not done here

- No coverage gate or threshold in CI. That decision needs this baseline first.
- No upload to a coverage service.
- No tests written to raise the number.
