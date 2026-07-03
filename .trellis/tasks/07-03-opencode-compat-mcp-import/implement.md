# Implementation plan

1. Update `crates/hya/tests/frontend_cli.rs` import test to include one local MCP and one remote MCP. Assert that `mcp import: TODO` is absent and the written hya config contains the local MCP only.
2. Run `cargo test -p hya --test frontend_cli import_compat_imports_model_config_without_tty`; expected initial result: fail because MCP import is still TODO.
3. Extend `CompatModelConfig` or introduce a broader Compat import config to deserialize `mcp`.
4. Add local MCP import structs and mapping to `McpServerConfig`.
5. Extend `CompatImportSummary` with imported/skipped MCP counts and update CLI output.
6. Update config render/merge so imported MCP entries are serialized and existing non-model sections remain intact.
7. Update `docs/configuration.md` to remove the MCP TODO language and document local-only import.
8. Bump release metadata to `0.29.4` and archive current root changelog to `docs/changes/CHANGELOG_0.29.2.md` in this branch.
9. Run:

```sh
cargo test -p hya --test frontend_cli
cargo test -p hya-app
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

10. Commit with `feat(config): import compat local mcp servers`.
