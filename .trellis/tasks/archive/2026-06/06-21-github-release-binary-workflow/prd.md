# GitHub release binary workflow

## Goal

Create a GitHub Actions release workflow for the Rust workspace so publishing a version tag builds the `yaca` release binary, creates/updates a GitHub Release, and uses the current root `CHANGELOG.md` as the release-note body.

The local-agent release rule must be explicit in the repository: before a new version is tagged, the local agent writes that version's changelog into root `CHANGELOG.md`; root `CHANGELOG.md` contains only the newest version; older version changelogs are archived under `docs/changes/CHANGELOG_<version>.md`.

## Confirmed Facts

- The repository is a Rust workspace with an existing CI workflow at `.github/workflows/ci.yml`.
- The release binary is the `yaca` binary declared in `crates/yaca-cli/Cargo.toml`.
- The workspace version lives at `[workspace.package].version` in root `Cargo.toml`.
- There is no existing root `CHANGELOG.md`, `docs/changes/`, or release workflow.
- The existing CI uses `dtolnay/rust-toolchain@stable` and `Swatinem/rust-cache@v2`.
- `softprops/action-gh-release` supports tag-push release workflows, `body_path`, asset uploads, and requires `contents: write` permission.

## Requirements

- Add a GitHub Actions workflow dedicated to release publishing.
- Trigger release publishing from version tags using the repository's release tag convention.
- Build the `yaca` binary in release mode before publishing.
- Package the built binary into a downloadable release asset.
- Publish a GitHub Release for the triggering tag.
- Use root `CHANGELOG.md` verbatim as the GitHub Release body.
- Fail the workflow before publishing if `CHANGELOG.md` is missing or empty.
- Add repository-local agent guidance that requires newest-only `CHANGELOG.md` and archives historical changelogs under `docs/changes/CHANGELOG_<version>.md`.
- Bootstrap the changelog files/directories needed for the first release process.
- Keep the release workflow independent from unrelated CI changes and existing dirty worktree changes.

## Out of Scope

- No crates.io publishing.
- No code signing or notarization.
- No automatic changelog generation inside GitHub Actions.
- No version bump automation.
- No migration of unrelated CI jobs.
- No changes to Rust production code.

## Acceptance Criteria

- [ ] `.github/workflows/release.yml` exists and runs on version tag pushes.
- [ ] The workflow installs the Rust toolchain, builds `cargo build --release --bin yaca`, packages the resulting binary, uploads the build artifact, then publishes/updates a GitHub Release.
- [ ] The release job uses `CHANGELOG.md` as the release body and rejects missing or empty changelog content.
- [ ] `CHANGELOG.md` exists at the repository root and is documented as newest-version-only.
- [ ] `docs/changes/` exists as the historical changelog archive location.
- [ ] `AGENTS.md` documents the local-agent release rule outside the Trellis-managed block.
- [ ] Workflow syntax is validated locally where tooling is available.
- [ ] The Rust release binary build is verified locally with the same binary target used by the workflow.

## Notes

- Planning assumption for review: start with a conservative Linux x86_64 release asset unless implementation review or user feedback requires a wider matrix.
