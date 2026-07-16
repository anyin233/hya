# Compat MCP config import

## Goal

Extend hya Compat import so local stdio MCP server entries migrate into hya config while preserving existing non-model sections.

## Requirements

- Extend `hya --import compat` so OpenCode/Compat local stdio MCP server config entries are imported into hya config.
- Preserve existing non-model hya config sections during import.
- Skip remote/OAuth MCP entries explicitly because hya does not yet implement remote MCP transport or OAuth.
- Keep current provider/model import behavior intact.

## Acceptance Criteria

- [x] A red CLI integration test proved local MCP entries were not imported and the CLI printed `mcp import: TODO`.
- [x] Local MCP config with `type: "local"`, `command`, `environment`, `enabled`, and `timeout` becomes hya `mcp.<name>.command/env/enabled/timeout_ms`.
- [x] Remote/OAuth MCP entries are skipped and counted without being serialized as fake local configs.
- [x] Existing hya-only MCP entries and unrelated config sections survive import, including MCP-only imports.
- [x] Version `0.33.10` metadata, full Rust CI-equivalent checks, and local binary builds are complete.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
