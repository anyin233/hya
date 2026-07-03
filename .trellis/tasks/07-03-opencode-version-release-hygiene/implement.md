# Implementation plan

1. Add `crates/hya/tests/version_metadata.rs` with assertions for README, root CHANGELOG, and root Cargo manifest.
2. Run `cargo test -p hya --test version_metadata`; expected initial result: fail on README version drift.
3. Update root `Cargo.toml` version to `0.29.3`.
4. Refresh `Cargo.lock` package versions with `cargo metadata --format-version=1`.
5. Move current root changelog to `docs/changes/CHANGELOG_0.29.2.md`.
6. Replace root `CHANGELOG.md` with a `# 0.29.3` note for the version metadata hygiene fix.
7. Update README active-development version to `0.29.3`.
8. Run:

```sh
cargo test -p hya --test version_metadata
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

9. Commit with `fix: align release metadata version`.
