---
kind: AgentBundle
identity:
  id: hya/docs-bun-transient
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agent:
  id: docs-bun-transient
  role: main
  spawn_lifecycle: transient
  resource_view:
    allow:
      - echo
---

Harness remains the agent and model loop; this activation sidecar supplies only the Bundle-local tools.
