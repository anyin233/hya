# GitHub Release Binary Workflow Design

## Summary

Add a release-only GitHub Actions workflow that runs on version tag pushes, builds the `hya` CLI binary, packages a Linux x86_64 release asset, and publishes a GitHub Release whose body is the root `CHANGELOG.md` content.

The repository-local release contract is agent-driven: before a release tag is pushed, the local agent must write the new version's notes into root `CHANGELOG.md`, keep that file newest-version-only, and archive the previous version's notes under `docs/changes/CHANGELOG_<version>.md`.

## Merged Planning Decisions

### Trigger

- Use `on.push.tags` with `v*.*.*`.
- Do not use `release.published`; this workflow should create/update the GitHub Release itself so binaries and notes are published together.
- A manual `workflow_dispatch` fallback is out of scope for this task.

### Build scope

- Build one first-release asset: `hya-${version}-x86_64-unknown-linux-gnu.tar.gz`.
- Run on `ubuntu-22.04` instead of `ubuntu-latest` to reduce glibc drift for Linux users.
- Defer macOS, Windows, musl, signing, and notarization to future tasks. The user requested a release binary, not a platform matrix.

### Release notes

- Use `CHANGELOG.md` verbatim as the GitHub Release body via `softprops/action-gh-release` `body_path`.
- Do not parse a subsection from `CHANGELOG.md`; the local-agent rule makes the entire file the newest version's release notes.
- Fail before publishing if `CHANGELOG.md` is missing or empty.

### Version safety

- Validate that the pushed tag minus a leading `v` equals the Cargo package version reported for `hya-cli` by `cargo metadata`.
- Smoke test the packaged binary with `--version` and `--help`; `--version` must contain the tag version.
- These checks prevent releasing an asset whose binary version does not match the GitHub tag.

### Permissions and release hardening

- Set workflow-level `permissions: contents: read`.
- Grant the `build` job only the extra `id-token: write` and `attestations: write` permissions required for GitHub build provenance attestations.
- Elevate only the `release` job to `permissions: contents: write` for GitHub Release publishing.
- Put the `release` job behind the `release` environment so repository settings can require reviewer approval before publish.
- Pin third-party actions to immutable commit SHAs; GitHub-owned actions may remain on major versions.
- Do not grant package or write-all permissions.

## Workflow Structure

### `build` job

1. Check out the tag ref.
2. Install stable Rust with the `x86_64-unknown-linux-gnu` target through the pinned `dtolnay/rust-toolchain` action SHA.
3. Restore Rust cache using the pinned `Swatinem/rust-cache` action SHA.
4. Validate tag version shape and match it against `cargo metadata` for the `hya-cli` package.
5. Validate `CHANGELOG.md` exists, is non-empty, and its first heading matches the tag version.
6. Run `cargo build --release --locked --bin hya --target x86_64-unknown-linux-gnu`.
7. Stage the binary under a versioned directory and include `README.md`.
8. Create a `.tar.gz` release archive and `SHA256SUMS`.
9. Extract the archive to a temporary directory and run `hya --version` and `hya --help` from the packaged copy.
10. Create build provenance attestations for the archive and checksum.
11. Upload the archive and checksum as one uniquely named workflow artifact.

### `release` job

1. Wait for `build` to succeed.
2. Check out the tag ref so `CHANGELOG.md` is available.
3. Revalidate `CHANGELOG.md` exists and is non-empty.
4. Download release assets into `dist/`.
5. Require the `release` environment before the publishing step can proceed.
6. Invoke the pinned `softprops/action-gh-release` action SHA once with `body_path: CHANGELOG.md` and `files: dist/*`.

## Changelog and Agent Rule

Add a `## Release & Changelog Rule` section to root `AGENTS.md` outside the Trellis-managed block:

- Before publishing a new version, the local agent must move the previous root `CHANGELOG.md` content to `docs/changes/CHANGELOG_<version>.md` when applicable.
- The local agent must then rewrite root `CHANGELOG.md` with only the new version's changelog.
- The local agent must ensure `[workspace.package].version` in `Cargo.toml`, the tag `vX.Y.Z`, and the `CHANGELOG.md` content describe the same version before the tag is pushed.
- The GitHub Actions release workflow reads root `CHANGELOG.md` verbatim for the GitHub Release notes.

## Files to Modify or Create

- Create `.github/workflows/release.yml`.
- Create `CHANGELOG.md` with a current-version stub for `0.0.0`.
- Create `docs/changes/.gitkeep`.
- Modify `AGENTS.md` outside the Trellis-managed block.

## Failure Modes and Rollback

- If tag/version/changelog validation fails, the workflow exits before any release object is created.
- If build, package, checksum, or packaged-binary smoke tests fail, the release job is skipped.
- If publishing succeeds with bad content, rollback is manual: delete the GitHub Release and remote tag, then cut a new version tag rather than reusing cached asset URLs.

## Out of Scope

- Multi-platform release matrix.
- Code signing, notarization, installers, package managers, or crates.io publishing.
- Generating changelogs from git history inside GitHub Actions.
- Adding license files that do not currently exist in the repository.
