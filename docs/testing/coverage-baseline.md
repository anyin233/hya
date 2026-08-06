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

## Track P's contribution — measured, and why the number is not publishable

**Attempted, and the answer is that it cannot be measured as the harness stands.**
This section records the evidence instead of a misleading figure.

### What was tried

No source change was needed. `cargo llvm-cov show-env --sh` exports the
instrumentation environment; building with the **default** target dir then puts
an instrumented `hya-backend` at `target/debug/hya-backend`, which is exactly
where `default_backend_bin()` looks. The spawned child inherits
`LLVM_PROFILE_FILE`, so in principle it emits its own profile data.

```sh
cargo llvm-cov clean --workspace
eval "$(cargo llvm-cov show-env --sh)"
cargo build --bin hya-backend          # instrumented, into target/debug
cargo test -p hya-e2e -- --test-threads=1
cargo llvm-cov report --summary-only
```

All 18 Track P test binaries passed, and 6 `.profraw` files were produced.

### The result, and why it is wrong

| Crate | Line coverage |
| ---: | --- |
| hya-backend | 23.9% |
| hya-store | 8.8% |
| hya-app | 0.2% |
| **hya-server** | **0.0%** |
| **hya-core** | **0.0%** |
| hya-plugin | 0.0% |

`hya-server` and `hya-core` at 0.0% is self-contradictory: those crates *are*
what serves every prompt, tool call, and session in a Track P run. A report
claiming Track P contributes 1.64% of lines would be false.

### Root cause

`crates/hya-e2e/src/backend.rs`:

```rust
impl Drop for BackendProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        …
```

`std::process::Child::kill()` sends **SIGKILL** on Unix. A SIGKILL'd process
never runs atexit handlers, and LLVM writes `.profraw` from an atexit handler.
So every `serve` process is destroyed before it can flush its coverage data.

The 23.9% / 8.8% that *did* appear comes from the short-lived CLI invocations
that exit normally — `hya-backend bundle install/list/info/uninstall` in
`p11_hyabundle.rs`. Those are real; the serving path is simply absent.

### What would make it measurable

The harness would have to stop the backend gracefully — SIGTERM plus a wait for
normal exit, with the backend handling SIGTERM by returning from `main` — the
way `crates/hya-sdk/src/server.rs` already does it (SIGTERM to the process
group, ~1s grace, then SIGKILL). That is a harness change, out of scope for a
measurement task, and it is a prerequisite for any future Track P coverage
number.

Until then: **the workspace figure above excludes Track P entirely, and Track P's
contribution is unknown.** `hya-client`'s 0.0% in the table above is the visible
symptom of that hole.

## Earlier analysis (superseded by the measurement above)

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
