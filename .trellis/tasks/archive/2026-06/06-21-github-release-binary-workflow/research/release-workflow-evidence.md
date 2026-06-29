# Release Workflow Evidence

## Repository facts

- Existing CI lives at `.github/workflows/ci.yml` and uses `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, and `cargo test --workspace`.
- The workspace binary to release is `hya`, declared by `[[bin]]` in `crates/hya-cli/Cargo.toml`.
- Workspace versioning is centralized in root `Cargo.toml` at `[workspace.package].version`.
- No root `CHANGELOG.md`, `docs/changes/`, or release workflow exists before this task.

## External action contracts checked

- `softprops/action-gh-release` supports tag-push workflows, release asset uploads via `files`, external release notes via `body_path`, and requires `permissions: contents: write`.
- `softprops/action-gh-release` can create a release or update an existing release for the tag; the body can come directly from `CHANGELOG.md`.
- `actions/upload-artifact` requires unique artifact names in matrix jobs and warns that plain zipped artifacts do not preserve executable permissions; prebuilt `.tar.gz` / `.zip` release archives avoid relying on artifact extraction permissions.
- `actions/download-artifact` can download multiple artifacts into one directory with `pattern` and `merge-multiple: true`.
- GitHub recommends least-privilege `GITHUB_TOKEN` permissions; checkout-only build jobs can use `contents: read`, and the release publishing job needs `contents: write`.

## Planning decisions to carry forward

- Use tag pushes (`v*.*.*`) rather than `release.published` so the workflow creates/updates the release and attaches assets in one run.
- Use root `CHANGELOG.md` verbatim as the release body; do not generate or parse release notes inside CI.
- Put the local-agent changelog process rule in `AGENTS.md` outside the Trellis-managed block so future local agents read it.
- Bootstrap `docs/changes/` with `.gitkeep` because the first release may not have history yet.
