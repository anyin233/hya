# Refine configurable permission policies

## Goal

Make permission defaults predictable and configurable across built-in tools, MCP tools, shell commands, and subagent spawning.

## Background

- Read-only tools should be allowed without prompting by default.
- Subagent spawning should be allowed without prompting by default.
- Configuration should support regex selectors with `Allow`, `Deny`, and `Ask` decisions.
- The requested policy modes are `allow`, `default`, `strict`, and `danger`.
- The existing permission plane evaluates action/resource rules and owns ask, deny, and remembered-allow behavior.
- Native configuration already has one typed YAML-to-runtime path; permission configuration will use that path.
- MCP tools already identify themselves by namespaced names, while plugin tools currently have no permission assertion of their own.

## Requirements

- Add a permission section to the application configuration.
- Define the YAML contract as `permission.model` plus an ordered `permission.rules` list; each rule has `target: tool|mcp|command`, a regex `selector`, and `permission: Allow|Deny|Ask`.
- Let configured rules select built-in tools, MCP tools, and shell commands using regular expressions.
- Define `tool` selectors over canonical registered built-in and plugin tool names; MCP tools use the separate `mcp` selector domain.
- Let each matching rule produce one of three decisions: `Allow`, `Deny`, or `Ask`.
- In `allow` mode, deny when any deny rule matches and otherwise allow; configured allow/ask rules do not narrow this mode.
- In `default` mode, let the last matching configured rule win; when unmatched, allow read-only tools and subagent spawning and ask for every other tool, MCP call, or command.
- In `strict` mode, ask for each registered tool, MCP call, and shell command unless that exact target was previously approved with `AllowAlways`; explicit denies remain denied.
- In `danger` mode, allow every operation and ignore configured rules, including denies.
- Treat local read-only built-ins (`read`, `ls`, `glob`, `find`, `grep`, `lsp`, `skill`, `list_agents`, `roster`, and `channels`) as allowed by the default policy.
- Keep network reads (`webfetch` and `websearch`) ask-by-default.
- Treat subagent spawning as allowed by the default policy.
- Preserve trust-boundary validation for invalid regexes and unsupported configuration values.
- Keep interactive `Ask` decisions on the existing permission request/event path.
- Do not silently approve an `Ask` in a headless flow that cannot service it.
- For native invocation asks, `AllowAlways` remembers only the exact canonical tool name, MCP name, or command text; explicit denies still win. Legacy action/resource approvals retain their current scope.

## Acceptance Criteria

- [ ] Configuration accepts documented regex-based permission rules for tools, MCP tools, and commands.
- [ ] Each mode resolves representative allowed, denied, asked, and unmatched operations according to its contract.
- [ ] The documented local read-only built-ins and subagent spawning resolve to allowed under the default policy without explicit rules; network reads resolve to ask.
- [ ] Invalid permission configuration fails through the existing configuration error path.
- [ ] Existing permission prompts and event flow continue to operate for `Ask` decisions.
- [ ] Native `AllowAlways` suppresses a later ask only for the same exact target and cannot bypass an explicit deny.
- [ ] Focused tests cover rule matching, precedence, mode fallbacks, read-only defaults, and subagent spawning.

## Configuration Contract

```yaml
permission:
  model: default
  rules:
    - target: tool
      selector: "^(read|grep)$"
      permission: Allow
    - target: mcp
      selector: "^mcp__github__"
      permission: Ask
    - target: command
      selector: "^git (status|diff)"
      permission: Allow
```

- Rule order is significant only in `default` mode, where the last match wins.
- Regexes use standard Rust `regex` matching; callers use anchors when full-string matching is required.
- Omitting `permission` selects the `default` model with no configured rules.
