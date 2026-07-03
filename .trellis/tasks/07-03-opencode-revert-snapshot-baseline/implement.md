# Implementation plan

1. Add a failing test in `compat_session_revert_api.rs` asserting that after an `edit` turn, `/revert` changes the target file back to `old\n`, and `/unrevert` changes it to `new\n`.
2. Run the targeted test and confirm it fails because current revert is metadata-only.
3. Add an `edit` tool unit test proving `metadata.filediff.beforeContent` and `afterContent` are emitted.
4. Update `crates/hya-tool/src/edit.rs` to include before/after snapshot fields.
5. Add restore helpers in `session_revert.rs` that replay events, filter by `DiffTarget`, collect optional snapshots, and write before/after content to safe workdir-relative paths.
6. Call restore helpers from `revert` and `unrevert` around the existing metadata update flow.
7. Bump release metadata to `0.29.6` and archive current root changelog to `docs/changes/CHANGELOG_0.29.2.md` in this branch.
8. Run:

```sh
cargo test -p hya-server --test compat_session_revert_api
cargo test -p hya-tool --test edit
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

9. Commit with `feat(server): restore edit snapshots on session revert`.
