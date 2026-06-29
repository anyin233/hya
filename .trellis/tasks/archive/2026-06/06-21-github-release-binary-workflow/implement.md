# GitHub Release Binary Workflow Implementation Plan

**Goal:** Add a tag-triggered GitHub Actions release workflow that builds and publishes the `hya` binary with release notes read from newest-only `CHANGELOG.md`.

**Architecture:** A build job performs all validation, compilation, packaging, checksum generation, and packaged-binary smoke tests. A separate release job runs only after build success, downloads the prepared assets, and publishes one GitHub Release using `CHANGELOG.md` as `body_path`.

**Tech Stack:** GitHub Actions YAML, Rust/Cargo, pinned `dtolnay/rust-toolchain`, pinned `Swatinem/rust-cache`, `actions/upload-artifact`, `actions/download-artifact`, `actions/attest-build-provenance`, and pinned `softprops/action-gh-release`.

---

## Scenario Contract

### Scenario 1: Happy path release tag

- **Surface:** GitHub Actions workflow plus local shell simulation of its build/package/smoke steps.
- **Pass condition:** A `v0.0.0`-style tag would build `target/x86_64-unknown-linux-gnu/release/hya`, package `hya-0.0.0-x86_64-unknown-linux-gnu.tar.gz`, produce `SHA256SUMS`, and publish a GitHub Release body from `CHANGELOG.md`.
- **Verification:** `actionlint .github/workflows/release.yml` if available; otherwise document tool absence. Run `cargo build --release --bin hya --target x86_64-unknown-linux-gnu`, package to a temporary directory, extract, run packaged `./hya --version`, and run packaged `./hya --help`.

### Scenario 2: Missing or empty changelog

- **Surface:** Release workflow validation script.
- **Pass condition:** Missing or empty `CHANGELOG.md` exits non-zero before release publication.
- **Verification:** Run the validation shell snippet against a temporary empty changelog copy or inspect the workflow with `actionlint`; do not modify the real committed `CHANGELOG.md` to force failure.

### Scenario 3: Adjacent CI regression

- **Surface:** Existing project CI commands.
- **Pass condition:** The new release workflow does not modify `.github/workflows/ci.yml` or Rust production code, and `cargo build --release --bin hya --target x86_64-unknown-linux-gnu` succeeds locally.
- **Verification:** Confirm `git diff -- .github/workflows/ci.yml crates Cargo.toml Cargo.lock` contains no unrelated edits from this task; run the release binary build command.

## TDD / Test Exemption

No Rust unit test is added because the implementation changes GitHub Actions configuration and documentation only. The behavioral lock is the workflow validation plus real-surface build/package/smoke execution.

## Implementation Checklist

### Task 1: Add root changelog and archive directory

**Files:**
- Create: `CHANGELOG.md`
- Create: `docs/changes/.gitkeep`

**Steps:**
1. Add root `CHANGELOG.md` with the current workspace version `0.0.0` and a concise initial changelog entry.
2. Add `docs/changes/.gitkeep` so historical changelog archive path exists before any previous release history exists.
3. Verify `CHANGELOG.md` is non-empty.

### Task 2: Add local-agent release rule

**Files:**
- Modify: `AGENTS.md`

**Steps:**
1. Append a `## Release & Changelog Rule` section after the Trellis-managed block.
2. State that root `CHANGELOG.md` must contain only the newest version's changelog.
3. State that previous changelogs must move to `docs/changes/CHANGELOG_<version>.md` before writing the new root changelog.
4. State that the release workflow reads root `CHANGELOG.md` verbatim for GitHub Release notes.
5. State that `[workspace.package].version`, the release tag, and root `CHANGELOG.md` must describe the same version before pushing a release tag.

### Task 3: Add release workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Workflow requirements:**
- `name: release`
- Trigger: `push` tags matching `v*.*.*`
- Top-level `permissions: contents: read`
- Env: `CARGO_TERM_COLOR: always`, `RUSTFLAGS: "-D warnings"`, `BINARY_NAME: hya`, `TARGET: x86_64-unknown-linux-gnu`
- `build` job on `ubuntu-22.04`, job-level `permissions: contents: read`, `id-token: write`, and `attestations: write`
- `release` job on `ubuntu-22.04`, `needs: build`, `environment: release`, and job-level `permissions: contents: write`
- `concurrency` serializes runs per triggering ref with `cancel-in-progress: false`

**Build job steps:**
1. `actions/checkout@v4`
2. Pinned `dtolnay/rust-toolchain` action SHA with `targets: x86_64-unknown-linux-gnu`.
3. Pinned `Swatinem/rust-cache` action SHA.
4. Validate tag version shape using a semver regex, then validate it using `cargo metadata --no-deps --format-version 1` and Python JSON parsing for package `hya-cli`.
5. Validate `CHANGELOG.md` with `test -s CHANGELOG.md` and require the first heading to equal `# <version>`.
6. Build: `cargo build --release --locked --bin hya --target "$TARGET"`.
7. Package a versioned directory under `dist/`, copy the binary and `README.md`, create `.tar.gz`, and create `SHA256SUMS`.
8. Extract the `.tar.gz` to a temp directory; run packaged `hya --version` and grep the version; run packaged `hya --help`.
9. Attest `dist/*.tar.gz` and `dist/SHA256SUMS` with `actions/attest-build-provenance`.
10. Upload `dist/*.tar.gz` and `dist/SHA256SUMS` with `actions/upload-artifact@v4`, unique artifact name, and `if-no-files-found: error`.

**Release job steps:**
1. `actions/checkout@v4`
2. `test -s CHANGELOG.md`
3. Download artifact with `actions/download-artifact@v4`, `pattern: release-assets-*`, `path: dist`, `merge-multiple: true`
4. Publish with pinned `softprops/action-gh-release` action SHA, `body_path: CHANGELOG.md`, `files: dist/*`, `fail_on_unmatched_files: true`

### Task 4: Verify workflow and release surface

**Commands:**
- `actionlint .github/workflows/release.yml` when `actionlint` is available.
- `cargo build --release --locked --bin hya --target x86_64-unknown-linux-gnu`
- Local package smoke script mirroring the workflow archive/extract/run flow in `/tmp`, using the current Cargo version.
- `git diff -- .github/workflows/ci.yml` to confirm existing CI was not changed.

**Expected results:**
- Workflow lints clean, or tool absence is explicitly reported.
- Cargo release build exits 0.
- Packaged binary `--version` includes the workspace version.
- Packaged binary `--help` exits 0.
- Existing CI workflow diff is empty.

## Rollback Points

- Before publishing: validation/build/smoke failures create no GitHub Release.
- After local edits: revert only this task's files (`.github/workflows/release.yml`, `CHANGELOG.md`, `docs/changes/.gitkeep`, `AGENTS.md`, and Trellis task artifacts) if implementation is abandoned.
- After an accidental real release: delete the GitHub Release and remote tag, then publish a new version tag instead of reusing the old one.

## Plan Review

### Round 1 — Oracle — VERDICT: PASS

D1 PASS  # Goal, Out of Scope, and 8 falsifiable Acceptance Criteria in prd.md:3-49 are concrete (workflow file path, build command, body_path, empty-changelog rejection).
D2 PASS  # implement.md:37-103 decomposes into 4 ordered tasks; each is 1–3 tool calls (Write CHANGELOG.md+.gitkeep, edit AGENTS.md, Write release.yml, run 4 verify commands).
D3 PASS  # All cited surfaces exist and resolve: ci.yml uses the same actions [.github/workflows/ci.yml:15-19]; hya bin [crates/hya-cli/Cargo.toml:8-10]; workspace version 0.0.0 [Cargo.toml:8]; `hya --version` is derived by clap from CARGO_PKG_VERSION [crates/hya-cli/src/main.rs:45-50]; assumption (Linux x86_64 only) is named [prd.md:53].
D4 PASS  # design.md:80-84 + implement.md:106-109 enumerate validation-before-publish, build-failure → release-job-skipped, and post-publish rollback (delete release + tag, cut new tag); `test -s CHANGELOG.md` is the explicit abort gate.
D5 PASS  # 3 scenario contracts [implement.md:13-29] + Task 4 verification commands [implement.md:90-103] are concrete (actionlint, cargo build, packaged --version/--help, git diff CI). Minor gap: no explicit grep verifying AGENTS.md was edited, but covered implicitly by Acceptance Criteria.
D6 PASS  # Out-of-scope is enumerated in both prd.md:31-38 and design.md:86-91; Scenario 3 [implement.md:25-29] adds a `git diff` gate against ci.yml/crates/Cargo.toml/Cargo.lock to lock the blast radius; no Rust code touched.
VERDICT: PASS

## Verification Evidence

### Implementation Round 1 — 2026-06-21

- `ruby -e 'require "yaml"; YAML.load_file(".github/workflows/release.yml")'` plus release/build job assertions: PASS.
- Extracted every workflow `run` block from `.github/workflows/release.yml`, substituted GitHub expressions with `0.0.0`, and piped to `bash -n`: PASS.
- Workflow tag/version/changelog validation with `GITHUB_REF_NAME=v0.0.0`: PASS, wrote `version=0.0.0`.
- `target/tmp/actionlint/actionlint .github/workflows/release.yml`: PASS with `actionlint` 1.7.7 downloaded into ignored `target/tmp/actionlint/`.
- `RUSTFLAGS='-D warnings' cargo build --release --bin hya --target x86_64-unknown-linux-gnu`: PASS.
- Local release surface smoke test: packaged `target/x86_64-unknown-linux-gnu/release/hya` into `hya-0.0.0-x86_64-unknown-linux-gnu.tar.gz`, generated and verified `SHA256SUMS`, extracted the archive, ran packaged `hya --version`, and ran packaged `hya --help`: PASS.
- `cargo fmt --all --check`: PASS.
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS.
- `cargo test --workspace`: PASS; test suite reported all executed tests passing with one pre-existing ignored worktree test.
- Trellis task JSON and context JSONL validation: PASS.
- `.trellis/spec/backend/quality-guidelines.md` updated with the durable GitHub release binary workflow contract.

### Verification Refresh — 2026-06-21

- `target/tmp/actionlint/actionlint .github/workflows/release.yml`: PASS.
- Workflow YAML structure and Trellis task JSON/JSONL validation: PASS.
- `GIT_MASTER=1 git diff --check`: PASS.
- `RUSTFLAGS='-D warnings' cargo check --bin hya --target x86_64-unknown-linux-gnu`: PASS after a transient shared-worktree Rust mismatch resolved.
- `RUSTFLAGS='-D warnings' cargo build --release --bin hya --target x86_64-unknown-linux-gnu`: PASS.
- Local release surface smoke test: rebuilt `hya-0.0.0-x86_64-unknown-linux-gnu.tar.gz`, verified `SHA256SUMS`, extracted it, ran packaged `hya --version`, and ran packaged `hya --help`: PASS.
- `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`: PASS; test suite reported all executed tests passing with one pre-existing ignored worktree test.

### Security Review Hardening — 2026-06-21

- Security review initially failed on mutable third-party action tags, no protected publishing environment, and no build provenance.
- Resolved by pinning `dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `softprops/action-gh-release`, and `actions/attest-build-provenance` to commit SHAs.
- Added `concurrency` per release ref, `release` job `environment: release`, build job `id-token: write` plus `attestations: write`, and provenance attestation for `dist/*.tar.gz` plus `dist/SHA256SUMS`.
- Added semver-shaped tag validation, `CHANGELOG.md` first-heading version validation, `--locked` to the release build, and env-based handoff for GitHub expression outputs used inside shell scripts.
- `target/tmp/actionlint/actionlint .github/workflows/release.yml`: PASS after hardening.
- Hardened workflow YAML structure checks for build/release jobs, `environment: release`, and attestation permissions: PASS.
- Extracted every hardened workflow `run` block, substituted GitHub expressions, and piped to `bash -n`: PASS.
- Hardened workflow tag/version/changelog validation with `GITHUB_REF_NAME=v0.0.0`: PASS, wrote `version=0.0.0`.
- `RUSTFLAGS='-D warnings' cargo build --release --locked --bin hya --target x86_64-unknown-linux-gnu`: PASS.
- Local release surface smoke test after hardening: rebuilt `hya-0.0.0-x86_64-unknown-linux-gnu.tar.gz`, verified `SHA256SUMS`, extracted it, ran packaged `hya --version`, and ran packaged `hya --help`: PASS.
- `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`: PASS; test suite reported all executed tests passing with one pre-existing ignored worktree test.
- Security re-review after hardening: PASS; all prior blocking findings are resolved. Remaining notes are LOW residuals: the `release` environment must have protection rules configured in GitHub settings, pinned action SHAs need an update mechanism, and GitHub-owned actions remain on major tags.
