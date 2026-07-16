# Version and release metadata hygiene design

## Scope

Branch: `feat/opencode-version-release-hygiene`

Base: `main`

Assigned version: `0.33.9`

Primary files:

- `crates/hya/tests/version_metadata.rs`
- `README.md`
- `Cargo.toml`
- `Cargo.lock`
- `CHANGELOG.md`
- `docs/changes/CHANGELOG_0.33.8.md`
- `crates/hya-server/tests/compat_event_api.rs`

## Design

Create an integration test in the `hya` crate because `env!("CARGO_PKG_VERSION")` there resolves to the workspace package version used by the user-facing binary. The test reads the workspace root files by walking up from `CARGO_MANIFEST_DIR` and verifies:

- README contains `workspace version `<version>``.
- Root CHANGELOG first line is `# <version>`.
- Root Cargo manifest has `[workspace.package] version = "<version>"` by textual check.

The implementation updates release metadata to `0.33.9`, archives the `0.33.8` root changelog exactly once, and canonicalizes the macOS temporary-directory expectation so the unchanged baseline test is portable.

## Non-goals

- Do not introduce a new TOML parser dependency.
- Do not publish a tag or GitHub release.
- Do not edit historical changelog files except creating `CHANGELOG_0.33.8.md`.
