# Quality Guidelines

> Code quality standards for backend development.

---

## Overview

<!--
Document your project's quality standards here.

Questions to answer:
- What patterns are forbidden?
- What linting rules do you enforce?
- What are your testing requirements?
- What code review standards apply?
-->

(To be filled by the team)

---

## Forbidden Patterns

<!-- Patterns that should never be used and why -->

(To be filled by the team)

---

## Required Patterns

<!-- Patterns that must always be used -->

(To be filled by the team)

---

## Testing Requirements

<!-- What level of testing is expected -->

(To be filled by the team)

---

## Code Review Checklist

<!-- What reviewers should check -->

(To be filled by the team)

---

## Scenario: GitHub Release Binary Workflow

### 1. Scope / Trigger

- Trigger: any change that publishes release binaries, creates GitHub Releases, or modifies the release changelog process.
- Applies to `.github/workflows/release.yml`, root `CHANGELOG.md`, `docs/changes/`, root `AGENTS.md` release rules, and release-related task artifacts.

### 2. Signatures

- Release tag: `vX.Y.Z`, where `X.Y.Z` must match Cargo's `yaca-cli` package version.
- Cargo command: `cargo build --release --locked --bin yaca --target x86_64-unknown-linux-gnu`.
- Release archive: `yaca-<version>-x86_64-unknown-linux-gnu.tar.gz`.
- Checksum file: `SHA256SUMS` generated beside the release archive.

### 3. Contracts

- Root `CHANGELOG.md` contains only the newest version's release notes.
- Historical changelogs live under `docs/changes/CHANGELOG_<version>.md`.
- The GitHub Release body is read verbatim from root `CHANGELOG.md`.
- Release workflow permissions are read-only by default; only the release publishing job may request `contents: write`.
- Build provenance attestations are generated for the archive and checksum.
- Third-party release actions are pinned to immutable commit SHAs.
- The publishing job uses the `release` environment so repository settings can require manual approval.

### 4. Validation & Error Matrix

- Missing `v` tag prefix -> fail before build.
- Tag version is not semver-shaped -> fail before build.
- Tag version differs from `cargo metadata` package version for `yaca-cli` -> fail before build.
- Missing or empty `CHANGELOG.md` -> fail before publishing.
- `CHANGELOG.md` first heading differs from the tag version -> fail before build.
- Build, archive, checksum, or packaged-binary smoke failure -> skip release publishing.
- Missing release assets -> fail `softprops/action-gh-release` with `fail_on_unmatched_files: true`.

### 5. Good/Base/Bad Cases

- Good: `v0.1.0`, `[workspace.package].version = "0.1.0"`, root `CHANGELOG.md` contains only `0.1.0` notes, archive and checksum pass smoke checks.
- Base: first release has no historical changelog; keep `docs/changes/.gitkeep` and root `CHANGELOG.md` for the current version.
- Bad: appending old release notes to root `CHANGELOG.md`; this publishes stale history as the GitHub Release body.

### 6. Tests Required

- Parse workflow YAML, run `actionlint`, and syntax-check every embedded shell `run` block.
- Run the tag/version/changelog validation logic with a representative tag.
- Run the release build command for the configured target.
- Package the built binary, verify `SHA256SUMS`, extract the archive, and run packaged `yaca --version` plus `yaca --help`.
- Confirm third-party actions are pinned to commit SHAs and release publication uses the `release` environment.

### 7. Wrong vs Correct

#### Wrong

```yaml
permissions: write-all
```

```markdown
# CHANGELOG

## 0.2.0
- New release.

## 0.1.0
- Old release.
```

#### Correct

```yaml
permissions:
  contents: read

jobs:
  release:
    permissions:
      contents: write
```

```markdown
# 0.2.0

- New release.
```
