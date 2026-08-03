---
api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/docs-bun-disjoint
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/alpha.js
    - id: beta
      path: extensions/beta.js
  hooks:
    - id: event
      path: extensions/alpha.js
    - id: tool.execute.before
      path: extensions/beta.js
extensions:
  js:
    - id: alpha
      path: extensions/alpha.js
    - id: beta-runtime
      path: extensions/beta.js
agents:
  - local_id: docs-bun-alpha
    stable_id: docs-bun-alpha
    role: main
    prompt: prompts/alpha.md
    spawn_lifecycle: transient
    harness_access: full
    resource_view:
      allow:
        - echo
    can_spawn:
      - docs-bun-beta
      - docs-bun-static
    hook_refs:
      - event
  - local_id: docs-bun-beta
    stable_id: docs-bun-beta
    role: subagent
    prompt: prompts/beta.md
    spawn_lifecycle: resident
    harness_access: full
    resource_view:
      allow:
        - beta
    hook_refs:
      - tool.execute.before
  - local_id: docs-bun-static
    stable_id: docs-bun-static
    role: main
    prompt: prompts/static.md
    spawn_lifecycle: transient
    harness_access: basic
---
