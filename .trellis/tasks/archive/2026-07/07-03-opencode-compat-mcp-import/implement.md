# Implementation record

- [x] Add mixed provider/local/remote MCP CLI coverage and observe the expected `mcp import: TODO` red failure.
- [x] Deserialize Compat MCP entries and map supported local stdio fields into hya config.
- [x] Count and report imported/skipped entries; skip remote/OAuth entries explicitly.
- [x] Preserve unrelated config and hya-only MCP entries, including MCP-only import input.
- [x] Update configuration documentation and sequential version metadata to `0.33.10`.
- [x] Run targeted tests, the full Rust CI-equivalent gate, and local executable builds.
- [x] Commit as `691e3936` and safely push stacked PR #9 after fetching its target branches.
