---
kind: AgentBundle
identity:
  id: hya/docs-bun-resident
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
  id: docs-bun-resident
  role: main
  spawn_lifecycle: resident
  resource_view:
    allow:
      - echo
---

The healthy resident sidecar is activation-scoped and may keep volatile state; explicit stop and recovery semantics remain Harness-owned.
