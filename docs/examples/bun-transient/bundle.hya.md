---
api_version: hya.agent-bundle/v1
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
agents:
  - local_id: docs-bun-transient
    stable_id: docs-bun-transient
    role: main
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      allow:
        - echo
---

Harness remains the agent and model loop; this activation sidecar supplies only the Bundle-local tools.
