# AgentBundle Authoring (0.34.8)

Concise authoring guide for native `AgentBundle` sources. A prepare-valid Markdown
example lives at [`examples/bundle.hya.md`](examples/bundle.hya.md).

## Release boundaries

Release **0.34.8** does **not**:

- runtime-scan `docs/examples`, ordinary Markdown, or other repo paths for bundles
- install external bundles
- execute JS, Rust, MCP, tool, or hook references declared only in a Bundle

Built-in catalogs are prepared at **build time** and embedded. The executable boots
from those prepared bytes. A source that prepares successfully is not thereby
installable or runnable.

## Source forms

v1 accepts exactly one of:

- `bundle.yaml` multi-file directory sources (prompts, static skill content, …)
- a single `bundle.hya.md` with YAML frontmatter and Markdown body as the sole agent prompt

Both require the exact markers:

```yaml
api_version: hya.agent-bundle/v1
kind: AgentBundle
```

`bundle.hya.md` must declare exactly one agent and must not set `prompt:` in
frontmatter; the body is the prompt.

## Stable AgentName bytes

`stable_id` becomes the public `AgentName`. Those bytes are identity for events,
projection, replay, and spawn resolution. Treat them as stable wire values: do not
rename casually, and do not rely on `local_id` outside the bundle.

## Role vs spawn lifecycle

- `role` (`main` | `subagent`) controls **only** TUI direct-selector visibility:
  `main` is selectable in the TUI; `subagent` is hidden from direct TUI selection.
  Role does not grant spawn authority and does not place subagents into a
  separate TUI selector slot.
- `spawn_lifecycle` (`transient` | `resident`) is orthogonal: it only describes
  how Harness should spawn the entry when spawn is allowed. It does not change
  TUI direct-selector visibility.

## `can_spawn` reachability

`can_spawn` is the caller-facing reachability set. Agent-facing/internal roster
and ordinary spawn are derived only from the current caller's `can_spawn`
reachability, never from `role`. Omitted targets are not reachable; unknown or
denied targets fail closed. Bundles cannot invent reachability past the prepared
catalog.

## `harness_access` vs `resource_view`

- `harness_access`: `none` | `basic` | `full` — which Harness-owned resources enter
  the candidate set.
- `resource_view` (`allow` / `deny` / `aliases` / `namespace`) narrows and renames
  within that candidate set. Deny wins over allow.

A Bundle cannot expand `PermissionPlane` or plugin authority. Effective access is
the narrowing intersection of access, view, and Harness policy.

## Trust boundary

Bundle-declared executable code (when later releases support runners) is same-UID
trusted process code. There is **no** sandbox or malicious-code isolation claim.

## Built-ins and legacy

- Built-ins are prepared at build time from repo-native sources under
  `bundles/builtin/` and embedded; they are not discovered at runtime from disk.
- Legacy agent files (for example `.hya/agents/*.md` and former compat agent-file
  loaders) are **unsupported**. There is no migration, adapter, or dual catalog.

## Skills and unsupported features

- Bundle-local **static** skills may carry prepared content only. They are not a
  general skill plane, hot installer, or remote skill loader.
- Executable features without a current consumer (tool/MCP/hook/JS/Rust refs,
  `resource_profile`, and similar) return typed `UNSUPPORTED_BUNDLE_FEATURE`
  rather than being silently ignored.

## Example

See [`examples/bundle.hya.md`](examples/bundle.hya.md) for one flat `main` /
`transient` agent. Prepare it with the production preparer in tests or build
tooling; do not expect the runtime to scan or install it in 0.34.8.
