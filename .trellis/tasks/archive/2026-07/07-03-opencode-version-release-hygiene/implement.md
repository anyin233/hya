# Implementation record

- [x] Canonicalize the existing macOS temporary-workdir assertion and verify its targeted test.
- [x] Add `crates/hya/tests/version_metadata.rs`; observe the expected red failure on README drift.
- [x] Align Cargo, lockfile, README, and changelog metadata on version `0.33.9`.
- [x] Archive the previous root changelog as `docs/changes/CHANGELOG_0.33.8.md`.
- [x] Run the targeted test, full Rust CI-equivalent gate, and local executable builds.
- [x] Commit as `afd27c7b` and safely push PR #7 after fetching its target branch.
