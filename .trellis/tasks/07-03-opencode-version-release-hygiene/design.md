# Version and release metadata hygiene design

## Scope

Branch: `feat/opencode-version-release-hygiene`

Worktree: `.worktrees/opencode-version-release-hygiene`

Assigned version: `0.29.3`

Primary files:

- `crates/hya/tests/version_metadata.rs`
- `README.md`
- `Cargo.toml`
- `Cargo.lock`
- `CHANGELOG.md`
- `docs/changes/CHANGELOG_0.29.2.md`

## Design

Create an integration test in the `hya` crate because `env!("CARGO_PKG_VERSION")` there resolves to the workspace package version used by the user-facing binary. The test reads the workspace root files by walking up from `CARGO_MANIFEST_DIR` and verifies:

- README contains `workspace version `<version>``.
- Root CHANGELOG first line is `# <version>`.
- Root Cargo manifest has `[workspace.package] version = "<version>"` by textual check.

The implementation then updates release metadata to `0.29.3` and archives the current root changelog exactly once.

## Non-goals

- Do not introduce a new TOML parser dependency.
- Do not publish a tag or GitHub release.
- Do not edit historical changelog files except creating `CHANGELOG_0.29.2.md`.
