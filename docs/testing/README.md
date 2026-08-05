# Testing

hya verifies behavior at three complementary tracks. Prefer the lightest track
that can fail if a feature regresses; do not duplicate deep engine semantics in
process E2E when an in-process suite already owns them.

| Track | What runs | When to use |
| --- | --- | --- |
| **I** (in-process) | Crate `#[test]` / integration tests with `FakeProvider`, in-memory store, Axum router | Engine rules, projection, permissions math, Compat route shapes |
| **P** (process) | Real `hya-backend serve` + scripted OpenAI-compatible **FakeLlm** (`crates/hya-e2e`) | Product path: config → HTTP provider → tools → sessions → MCP/skills/subagents/hyabundle |
| **T** (TUI/SDK) | Bun tests against a real backend and pure presentation helpers (`packages/hya-tui-ts/test`) | SDK permission/question lifecycle, multi-agent presentation, roster visibility |

Machine registry of PR-matrix IDs: [`../../crates/hya-e2e/matrix.toml`](../../crates/hya-e2e/matrix.toml).

## Docs in this directory

| Page | Purpose |
| --- | --- |
| [Agent feature matrix](agent-matrix.md) | Tier 0–2 scenario inventory (Track P/T implemented + Track I index-only) |
| [Process E2E harness](process-e2e.md) | How `hya-e2e` builds environments, scripts FakeLlm, and asserts outcomes |
| [CI agent e2e snippet](ci-agent-e2e-snippet.yml) | Optional workflow fragment to run Track P after workspace tests |

## Default quality gate

From the workspace root (see also [Development](../development.md)):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Process agent E2E needs a built backend binary (not always present after a bare
`cargo test` matrix without prior build):

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```

Track T (non-PTY real-backend / presentation):

```sh
cargo build -p hya-backend --bin hya-backend
cd packages/hya-tui-ts
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

## Live model smoke (optional)

Live provider keys (`HYA_E2E_LIVE` and similar) are **not** part of the PR gate.
Use them only for manual or nightly smoke against a real provider.
