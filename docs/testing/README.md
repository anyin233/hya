# Testing

hya verifies behavior at two complementary tracks. Prefer the lightest track
that can fail if a feature regresses; do not duplicate deep engine semantics in
process E2E when an in-process suite already owns them.

| Track | What runs | When to use |
| --- | --- | --- |
| **I** (in-process) | Crate `#[test]` / integration tests with `FakeProvider`, in-memory store, Axum router | Engine rules, projection, permissions math, `hya.v1` route shapes and HTTP/gRPC parity (`crates/hya-server/tests/v1_api.rs`, `v1_grpc_parity.rs`) |
| **P** (process) | Real `hya-backend serve` + scripted OpenAI-compatible **FakeLlm** (`crates/hya-e2e`), driven entirely through the v1 client | Product path: config → HTTP provider → tools → sessions → MCP/skills/subagents/hyabundle |

There is no Track T (TUI) today: the legacy TypeScript TUI was removed and its
frontend suites retired with it; frontend-on-`hya-sdk-v1` coverage returns with
a future TUI.

Machine registry of PR-matrix IDs: [`../../crates/hya-e2e/matrix.toml`](../../crates/hya-e2e/matrix.toml).

## Docs in this directory

| Page | Purpose |
| --- | --- |
| [Agent feature matrix](agent-matrix.md) | T0–T3 scenario inventory (Track P implemented + Track I index-only) |
| [Process E2E harness](process-e2e.md) | How `hya-e2e` builds environments, scripts FakeLlm, and asserts outcomes |
| [CI wiring](ci-agent-e2e-snippet.yml) | Historical note — Track P is now enforced directly in `.github/workflows/ci.yml` |

## Default quality gate

From the workspace root (see also [Development](../development.md)):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --jobs 1 --exclude hya-e2e
```

`--exclude hya-e2e` matches CI: Track P spawns real backend processes and is run
separately below with `--test-threads=1`. CI also uses `--jobs 1` to cap
concurrent workspace-test resource use; local runs may omit that job cap.

CI exercises both tracks in different modes, but they are not all separate
gates: Track P is enforced; Track I remains an index-only classification within
the Rust suite. Each gate step carries
`if: ${{ !cancelled() }}`, so a failure in one step no longer skips the rest — a
red `fmt` used to abort the job before the test step ever ran, which hid six
failing tests for weeks.

Process agent E2E needs a built backend binary (not always present after a bare
`cargo test` matrix without prior build):

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```

## Coverage

Line-coverage baseline and how to regenerate it:
[coverage-baseline.md](coverage-baseline.md). The recorded workspace baseline is
**85.56% lines** (`hya-e2e` excluded), measured on 2026-08-05 at commit
`1a7db256`; it is not a current-HEAD coverage claim.

```sh
cargo llvm-cov --no-report --workspace --exclude hya-e2e --no-fail-fast
cargo llvm-cov report --summary-only
```

## Live model smoke (optional)

Live provider keys (`HYA_E2E_LIVE` and similar) are **not** part of the PR gate.
Use them only for manual or nightly smoke against a real provider.
