# 0.48.0-dev.38

- **Removed public Rust items deprecated in 0.48.0-dev.36 and 0.48.0-dev.37.** No runtime, wire or CLI behaviour changes; Rust embedders that still use these paths must move to the replacements named in the earlier release notes:
  - `hya-engine`: `TmuxPaneManager`, `resolve_compaction_decision`, `CompactionResolution`, and the public re-export of `handoff_request_messages`;
  - `hya-version`: `FRONTEND_VERSION` (use `VERSION`);
  - the Rust adapter identity constants `BUN_ADAPTER_VERSION`, `CLAUDE_ADAPTER_VERSION`, `CLAUDE_PLUGIN_KIND` and `PLUGIN_MANIFEST_FILE`; the `hya-plugin-bun` Rust crate is gone, while the Bun adapter stays at `crates/hya-plugin-bun/adapter` and ships as `lib/hya/bun-adapter`;
  - `hya-relay`: the public in-memory transport;
  - `hya-updater`: `RELEASE_INSTALLER` and `install_prefix`;
  - `hya-mcp`: `McpManager` (use `prepare` and `PreparedMcpServer`) and the `codec`/`io_tasks` re-exports (use `hya-child-io`);
  - `hya-tool`: the token accounting and rewrite re-exports (use `hya-context`);
  - `hya-workflow`: the old Workflow model type aliases (use the `hya-proto` types); `hya-server`: `valid_provider_id` (use `hya_provider::valid_provider_id`);
  - `hya-sdk-v1`: the methods taking `SessionId` or returning `Envelope` (use the DTO-based methods).
