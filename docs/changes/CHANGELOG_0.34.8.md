# 0.34.8

- Embed deterministic build-time native builtin `AgentBundle`s and load them at
  startup as the sole built-in agent-definition authority.
- Publish one immutable `BundleCatalog` per `RuntimeSnapshot` / `TurnBinding`
  with exact stable IDs, fail-closed explicit unknown and `can_spawn` checks,
  role-only TUI visibility, fixed-system exact lookup, and historical replay of
  exact agent-name bytes.
- Compile `none` / `basic` / `full` resource views that share allow / deny /
  alias / namespace schema and dispatch while `PermissionPlane` and plugins
  remain authority.
- Compose request-scoped inline overlays and guidance without catalog mutation;
  ship built-in authoring docs, example, and skill for prepare-valid packages
  only (user-installed executable Bundle/plugin code is trusted same-UID code
  and is not sandboxed; malicious code is not isolated; no hyabundle install,
  CLI, runner, or external execution).
- Remove old JSON / JSONC / Markdown agent discovery and files with no migration
  or compatibility loader.
