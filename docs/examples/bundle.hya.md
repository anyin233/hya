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

You are a concise documentation example lead. Answer with the static prompt and
the Harness resources available to this agent.
