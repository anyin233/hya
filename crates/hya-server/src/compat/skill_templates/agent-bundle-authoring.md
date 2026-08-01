<!--
  Built-in skill. Name and description are registered in
  skill_catalog.rs. The body below becomes the skill content.
-->

# AgentBundle authoring

Author and install a 0.34.10 public-static `AgentBundle`. Prefer one
`bundle.hya.md` with both v1 markers, exactly one agent, no frontmatter
`prompt:`, and no executable references. The Markdown body is the static agent
prompt.

Repository references:

- Authoring guide: `docs/agent-bundle-authoring.md`
- Public-static example: `docs/examples/bundle.hya.md`

## Package workflow

From an otherwise empty directory containing the root `bundle.hya.md`, use an
external `7z` only to create an unencrypted, non-solid, no-compression archive:

```sh
7z a -t7z -mx=0 -ms=off example.hyabundle bundle.hya.md
hya bundle info -f example.hyabundle
hya bundle install example.hyabundle
hya bundle list
hya bundle info <bundle-id>
hya bundle uninstall <bundle-id>
```

Inspect before installation. `hya bundle info -f` mutates neither registry nor
publication. Require the exact lowercase `.hyabundle` suffix; treat bytes magic
as public/private format authority. The runtime uses an in-process strict reader
and never shells to or depends on system `7z`. It does not runtime-scan docs,
ordinary Markdown, or other source directories.

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

- Install exactly one static agent definition and its Markdown prompt; the
  strict installable profile admits no external static-skill file.
- Keep the exact-one-entry public example to its agent declaration and Markdown
  prompt. Built-in or otherwise prepared catalogs may still expose static skill
  IDs and content, and `info` may report those IDs.
- Reject external tool/MCP/hook/JS/Rust execution references with typed
  `UNSUPPORTED_BUNDLE_FEATURE`; do not claim a runner or executable install.
- Private inspection means `authentication=unverified`, `payload=opaque`, and
  `activation unsupported-in-0.34.10`. Structural and declared-digest checks
  are not publisher authenticity.
- Built-ins are build-time prepared, read-only, and immutable.
- Installation adds no sandbox, malicious-code isolation, or new permission
  plane.
- Legacy agent files are unsupported; there is no migration path.

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

Package this single root file with the strict workflow above. Keep executable
references out of the 0.34.10 public-static package.
