---
kind: AgentBundle
identity:
  id: hya/docs-bun-disjoint
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/alpha.js
  hooks:
    - id: event
      path: extensions/alpha.js
extensions:
  js:
    - id: alpha
      path: extensions/alpha.js
agent:
  id: docs-bun-alpha
  role: main
  spawn_lifecycle: transient
  resource_view:
    allow:
      - echo
  hook_refs:
    - event
---

Disjoint activation closure: this agent selects exactly one Bundle-local tool
and one Bundle-local hook, both backed by the same JS extension entrypoint.

A bundle defines one agent, so a second specialist ships as a second bundle with
its own closure. Install both; neither can reach the other's tools.
