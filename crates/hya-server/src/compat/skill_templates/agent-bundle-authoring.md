<!--
  Built-in skill. Name and description are registered in
  skill_catalog.rs. The body below becomes the skill content.
-->

# AgentBundle authoring

Use this skill when authoring native `AgentBundle` sources (`bundle.yaml` or
`bundle.hya.md`).

## 0.34.8 prepare-only boundaries

Release 0.34.8 does not runtime-scan examples, install external bundles, or
execute JS/Rust/MCP/tool/hook refs. Built-ins are prepared at build time and
embedded. A prepare-valid docs example is not installed or run by the executable.

Repository references:

- Authoring guide: `docs/agent-bundle-authoring.md`
- Prepare-valid Markdown example: `docs/examples/bundle.hya.md`

## Required v1 markers

```yaml
api_version: hya.agent-bundle/v1
kind: AgentBundle
```

`bundle.hya.md` needs both markers, exactly one agent, and no frontmatter
`prompt:` — the Markdown body is the prompt.

## Stable AgentName bytes

`stable_id` is the public `AgentName`. Keep those bytes stable for events,
projection, replay, and spawn. Do not treat `local_id` as a public identity.

## Role only controls TUI direct-selector visibility

- `role: main` → selectable in the TUI direct selector
- `role: subagent` → hidden from direct TUI selection

Role never grants spawn authority and does not give subagents a TUI selector
placement. Agent-facing/internal roster and ordinary spawn are derived only from
the current caller's `can_spawn` reachability, never from `role`.

## `spawn_lifecycle` is orthogonal

`transient` / `resident` describes only how Harness spawns the entry when spawn
is allowed. It does not change TUI direct-selector visibility.

## `can_spawn` reachability

List only targets the caller may spawn. Omitted agents are unreachable. Unknown
or denied targets fail closed; there is no silent `general` fallback for
explicit unknown IDs. Ordinary roster and spawn use this set only, never `role`.

## `harness_access` vs `resource_view`

| Field | Meaning |
| --- | --- |
| `harness_access: none` | No Harness-owned resources enter candidates |
| `harness_access: basic` | Only the Harness basic builtin set enters |
| `harness_access: full` | All current Harness tool/skill/MCP resources enter |
| `resource_view.allow` / `deny` | Narrow the candidate set; deny wins |
| `resource_view.aliases` / `namespace` | Rename/resolve within the candidate set |

A Bundle cannot expand `PermissionPlane` or plugin authority.

## Trust and legacy boundaries

- Same-UID trusted code only; no sandbox / malicious-code isolation claim.
- Built-ins are build-time prepared; runtime does not re-scan authoring sources.
- Legacy agent files are unsupported; there is no migration path.
- Bundle-local static skills may embed prepared content only.
- Unsupported executable features return typed `UNSUPPORTED_BUNDLE_FEATURE`.

## Minimal Markdown shape

```markdown
---
api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: acme/example
  version: 1.0.0
  publisher: acme
agents:
  - local_id: lead
    stable_id: acme-example-lead
    role: main
    spawn_lifecycle: transient
    harness_access: full
---

You are the example lead agent.
```

Prepare through the production preparer. Do not claim installability or
runnability from prepare success alone in 0.34.8.
