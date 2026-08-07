# 0.35.1

- Fixed the bundle process-E2E suite, which had been failing since the
  one-agent-per-bundle change. That commit regenerated the public bundle fixture
  and renamed its ids (`hya/public-fixture` → `hya/valid-public`,
  `public-fixture-lead` → `valid-public-lead`), updating the backend and
  bundle-crate tests but not `crates/hya-e2e/tests/p11_hyabundle.rs`. The gap
  survived because `hya-e2e` is excluded from the default workspace test run.
- `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --all --check` pass again. `hya-sdk`, `hya-native`, and the `hya`
  integration tests used `unwrap`/`expect` in test code without the
  `#![allow(clippy::unwrap_used, clippy::expect_used)]` the rest of the workspace
  applies to its test modules. The allow is scoped to test code only — library
  and binary paths still deny both lints.
- `hya-sdk`'s `native_spike` example now reports a missing `HYA_BACKEND_DIR`
  through its `Result` instead of panicking, keeping the same operator-facing
  message.
