# 0.33.12

- Added Compat import support for local stdio MCP servers, including command, environment, enabled state, and timeout mapping.
- Preserved hya-only MCP and non-model configuration while skipping and reporting unsupported remote/OAuth MCP entries.
- Added CLI and config coverage for mixed provider/MCP imports and MCP-only imports.
- Quoted imported MCP and environment keys and escaped control characters so YAML-reserved names and multiline values round-trip safely.
