# Compat MCP config import

## Goal

Extend hya Compat import so local stdio MCP server entries migrate into hya config while preserving existing non-model sections.

## Requirements

- Extend `hya --import compat` so OpenCode/Compat local stdio MCP server config entries are imported into hya config.
- Preserve existing non-model hya config sections during import.
- Skip remote/OAuth MCP entries explicitly because hya does not yet implement remote MCP transport or OAuth.
- Keep current provider/model import behavior intact.

## Acceptance Criteria

- [ ] A red CLI integration test proves local MCP entries are not currently imported.
- [ ] Local MCP config with `type: "local"`, `command`, `environment`, `enabled`, and `timeout` becomes hya `mcp.<name>.command/env/enabled/timeout_ms`.
- [ ] Remote MCP entries are skipped and reported without being serialized as fake local configs.
- [ ] The import summary no longer prints `mcp import: TODO`.
- [ ] Assigned version `0.29.4` release metadata is updated.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
