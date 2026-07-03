# Compat MCP config import design

## Scope

Branch: `feat/opencode-compat-mcp-import`

Worktree: `.worktrees/opencode-compat-mcp-import`

Assigned version: `0.29.4`

Primary files:

- `crates/hya-app/src/config.rs`
- `crates/hya/tests/frontend_cli.rs`
- `docs/configuration.md`
- Release metadata files required by AGENTS.

## Config mapping

OpenCode local MCP shape:

```json
{
  "mcp": {
    "name": {
      "type": "local",
      "command": ["npx", "-y", "server"],
      "environment": { "TOKEN": "{env:TOKEN}" },
      "enabled": true,
      "timeout": 5000
    }
  }
}
```

hya target shape:

```yaml
mcp:
  name:
    command: ["npx", "-y", "server"]
    env:
      TOKEN: "{env:TOKEN}"
    enabled: true
    timeout_ms: 5000
```

Only `type: "local"` entries with non-empty command arrays are importable. Entries with `type: "remote"`, `url`, or missing local command are counted as skipped and left out of hya config.

## Merge behavior

The existing import replaces `default_model` and `providers` while preserving non-model sections. This change should merge imported MCP entries into the existing `mcp` map without deleting pre-existing hya-only MCP entries unless the imported entry has the same name.

## Non-goals

- Do not implement remote MCP transport or OAuth.
- Do not dynamically connect imported MCP servers during import.
- Do not import OpenCode MCP `cwd` until hya MCP config supports it.
