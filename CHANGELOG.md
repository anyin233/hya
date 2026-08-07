# 0.35.0

- **Breaking: an `AgentBundle` now defines exactly one agent.** The manifest key
  `agents:` (a list) becomes `agent:` (a map), `local_id` + `stable_id` collapse
  to a single `id`, and `api_version` and `harness_access` are removed. A
  manifest that still carries any of them is rejected by name. **Every installed
  bundle must be reinstalled after upgrading**; a row written by an older binary
  is skipped with a warning and shown as `unreadable (reinstall)` by
  `hya bundle list`.
- **A bundle agent's tool plane is host-controlled.** It is derived from the
  agent's origin instead of declared by the author: an installed bundle agent
  sees the internal public tool snapshot plus its own bundle resources, and
  never a tool installed at the main-agent level, a tool installed into hya
  directly, an MCP server's exports, or a project or user skill. Note that this
  bounds direct use only — a bundle agent may still spawn a built-in, which runs
  on its own full plane.
- Fixed a cross-bundle leak: a bundle could name `bundle:<other>/tool/x` in its
  `resource_view` and pull in another bundle's tool. Such a reference is now
  refused.
- Built-in agents left the bundle system. They are compiled into the binary, so
  `bundles/builtin/` and the `hya-app` build-time prepare step are gone, their
  ids are reserved against installed bundles, and an ordinary built-in can spawn
  any installed bundle agent without a configuration edit.
