---
api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/docs-example
  version: 1.0.0
  publisher: hya
agents:
  - local_id: lead
    stable_id: docs-example-lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
---

You are the minimal prepare-valid AgentBundle documentation example agent.

This file is prepare-valid source only. Release 0.34.8 does not runtime-scan
docs/examples, install external bundles, or execute JS/Rust/MCP/tool/hook refs.
Built-ins are prepared at build time; this example is not installed or run by
the executable.
