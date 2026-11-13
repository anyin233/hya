# Testing

hya verifies behavior at two complementary tracks. Prefer the lightest track
that can fail if a feature regresses; do not duplicate deep engine semantics in
process E2E when an in-process suite already owns them.

| Track | What runs | When to use |
| --- | --- | --- |
| **I** (in-process) | Crate `#[test]` / integration tests with `FakeProvider`, in-memory store, Axum router | Engine rules, projection, permissions math, `hya.v1` route shapes and HTTP/gRPC parity (`crates/hya-server/tests/v1_api.rs`, `v1_grpc_parity.rs`) |
| **P** (process) | Real `hya serve` + scripted OpenAI-compatible **FakeLlm** (`crates/hya-e2e`), driven entirely through the v1 client | Product path: config → HTTP provider → tools → sessions → MCP/skills/subagents/hyabundle |
| **TUI** (terminal & browser) | `packages/hya-tui` unit tests + `packages/hya-tui-web/tui-exp` headless Bun PTY + optional Playwright Chromium E2E (`packages/hya-tui-web/e2e/*.spec.ts`) | Interactive OpenTUI rendering, keybindings, resize, sessions, Projects, and WebUI PTY host ([tui-web.md](../tui-web.md)) |

Machine registry of PR-matrix IDs: [`../../crates/hya-e2e/matrix.toml`](../../crates/hya-e2e/matrix.toml).

## Docs in this directory

| Page | Purpose |
| --- | --- |
| [Agent feature matrix](agent-matrix.md) | T0–T3 scenario inventory (Track P implemented + Track I index-only) |
| [Process E2E harness](process-e2e.md) | How `hya-e2e` builds environments, scripts FakeLlm, and asserts outcomes |

## Local gate: affected components only

`hya-app` unit tests share one process environment/cwd gate in
`src/test_env.rs`. Hold `StableEnvGuard::new()` for the whole test when calling
code that reads environment or cwd, including engine construction, runtime
bindings, config/provider resolution, and subprocess setup. Readers can run
concurrently. A test that changes this state instead holds `EnvGuard::new()`
before its first read; `set(name, value)` and `remove(name)` restore the original
values on drop. `EnvGuard::with_home(home, cwd)` additionally creates the two
directories, sets `HOME`, `XDG_CONFIG_HOME`, and `XDG_DATA_HOME` to `home`, and
restores cwd on drop. Acquire only one guard per test.

For example, an engine fixture with a temporary home starts with
`let _env = crate::test_env::EnvGuard::with_home(&home, &workdir);` and retains
it until its engine is dropped. Separate module locks cannot isolate
process-global state: an unguarded engine can capture another test's registry
path and contend with its install while opening/migrating that database.

Locally, run the checks for the components a change touches (and the direct
dependents of a changed public contract), not the whole workspace. How to pick
them: [Development](../development.md#choosing-the-affected-components).

```sh
cargo fmt --all --check
cargo clippy -p <crate> [-p <dependent> ...] --all-targets -- -D warnings
cargo test -p <crate> [-p <dependent> ...]
```

## CI gate: full suite

CI runs the full suite on every push and pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
```

`--exclude hya-e2e` keeps Track P out of the multi-threaded workspace run; it
runs separately with `--test-threads=1`. Run this full gate locally only when
asked or for a cross-cutting change that touches most crates.

CI exercises both tracks in different modes, but they are not all separate
gates: Track P is enforced; Track I remains an index-only classification within
the Rust suite. CI runs lint (fmt, matrix check, clippy), the workspace suite,
Track P, and the TUI suites as four parallel jobs. Each gate step carries
`if: ${{ !cancelled() }}`, so a failure in one step no longer skips the rest — a
red `fmt` used to abort the job before the test step ever ran, which hid six
failing tests for weeks.

Process agent E2E needs a built backend binary (not always present after a bare
`cargo test` matrix without prior build). Locally, run only the scenario files
for the agent surface you changed:

```sh
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e --test <pNN_scenario> -- --test-threads=1
```

## Coverage

Generate a local workspace line-coverage summary with `cargo-llvm-cov` (run
collection and reporting in two steps so `--no-fail-fast` is accepted):

```sh
cargo llvm-cov --no-report --workspace --exclude hya-e2e --no-fail-fast
cargo llvm-cov report --summary-only
```

## Live model smoke (optional)

Live provider keys (`HYA_E2E_LIVE` and similar) are **not** part of the PR gate.
Use them only for manual or nightly smoke against a real provider.
