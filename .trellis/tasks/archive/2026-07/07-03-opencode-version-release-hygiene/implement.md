# Implementation record

- [x] Canonicalize the existing macOS temporary-workdir assertion and verify its targeted test.
- [x] Add `crates/hya/tests/version_metadata.rs`; observe the expected red failure on README drift.
- [x] Align Cargo, lockfile, README, root changelog, and packaged TypeScript TUI metadata on version `0.33.11`.
- [x] Archive the previous root changelog as `docs/changes/CHANGELOG_0.33.10.md`.
- [x] Align pinned TypeScript real-backend fixtures with exact permission patterns and isolate question lifecycle coverage from tool authorization.
- [x] Run available local TypeScript checks, the full Rust gate, and local executable builds; leave the pinned Bun runtime-boundary suite to PR CI.
- [x] Complete the reviewed PR at `a8c657bd` and safely push #7 after fetching its target branch.
