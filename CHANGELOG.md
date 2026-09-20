# 0.36.51

## Tool namespaces: `namespace__local` registration for tool groups

Introduces the tool-namespace mechanism in `hya-tool`. Provider
tool-name charsets only allow `[a-zA-Z0-9_-]`, so namespaces ride on a
double-underscore separator — the same convention MCP tools already use
(`mcp__server__tool`). This lets different tool groups contribute
same-named local tools (`todo__read`, `pluginx__read`) while full names
stay globally unique in a registry.

- `namespaced_name(namespace, local)` composes a canonical name after
  validating both tokens (non-empty, `[a-zA-Z0-9_-]`, no `__`);
  `namespace_of(name)` parses the namespace segment back out under a
  first-segment rule (MCP-compatible).
- `ToolRegistry::register_namespaced` /
  `register_namespaced_with_permission` register a tool under
  `namespace__local`, enforce that the tool's own name matches the
  composed canonical name, and reject duplicates with the typed
  `NamespacedRegisterError`.
- No builtin tool names change in this release; the first namespaced
  builtin group (`todo__*`) lands next.
