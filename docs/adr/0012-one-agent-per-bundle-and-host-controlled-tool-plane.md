# One agent per AgentBundle, with a host-controlled tool plane

An `AgentBundle` defines **exactly one** Agent, while a distinct
`WorkflowBundle` defines one Workflow and its exact reachable Agent closure.
Every bundle Agent's tool plane is derived from origin rather than declared in
its manifest.

Two things forced this. A bundle used to be both "a package of agents" and "an
agent definition", and the two roles pulled the format in opposite directions.
And an installed bundle could declare `harness_access: full`, which handed the
bundle author — not the host — the decision about how much of the machine the
agent could see.

## Decision

**An AgentBundle is one Agent.** Its prepared payload has one `agent` field, so
zero or multiple Agents are unrepresentable. Install one AgentBundle per
independent specialist.

**A WorkflowBundle is a separate closed payload.** It carries one compiled
Workflow and every Agent reachable from its Stages, verifiers, and recursive
`can_spawn` edges. Built-in ids are reserved rather than implicit closure
members, so a WorkflowBundle cannot depend on unpinned host Agent definitions.
This does not reopen the AgentBundle schema; the prepared package union
dispatches the two kinds explicitly.

**Core built-in Agents stay outside AgentBundle.** They remain Rust constants
under `crates/hya-core/src/builtin_agents/`. A build-prepared, read-only
first-party WorkflowBundle can contribute its own Workflow Agents through the
ordinary bundle catalog. `AgentCatalog` joins the compiled-in roster with the
bundle catalog behind one resolution seam, and rejects any bundle Agent that
shadows a core built-in id.

**Origin decides the tool plane.** `AgentToolPlane::for_origin` is the only
producer:

| Origin | Plane |
| --- | --- |
| Built-in | `Full` — the live tool snapshot, Harness skills, Harness MCP |
| Installed bundle | `InternalPublic` — the builtin tool snapshot captured at registry construction, plus the bundle's own resources, and nothing else |

`harness_access` and `api_version` are deleted from AgentBundle manifests, and
AgentBundle `agents:` becomes singular `agent:`. WorkflowBundle uses its own
strict `workflow:` plus `agents:` schema. Removed AgentBundle fields reject by
name so the author is told what to write instead.

## Consequences

- A bundle agent cannot see a tool installed at the main-agent level, a tool
  installed into hya directly, an MCP server's exports, or a project or user
  skill. A `resource_view` naming one fails with `ResourceNotInPlane`, which
  says "outside your plane" rather than "unknown".
- A bundle selects only its **own** resources. The previous allow-driven
  re-resolution searched every bundle, so bundle A could name
  `bundle:B/tool/x` and pull in B's tool; that is now refused.
- **The clamp is not a sandbox.** `can_spawn` may name a built-in, and a spawned
  built-in runs on its own full plane. The clamp bounds what a bundle agent does
  directly, not what it reaches by delegating. Document a bundle as a capability
  declaration, not a security boundary.
- An ordinary built-in spawns the whole ordinary set, so installing a bundle
  makes its agent spawnable with no edit to any built-in definition. Built-in
  agent ids are reserved; an install that claims one is rejected.
- `can_spawn` in an AgentBundle is resolved when a runtime roster is built, so
  one uninstalled cross-bundle target does not make another Agent unusable. A
  WorkflowBundle is different: preparation requires its complete recursive
  Agent closure and rejects missing, unreachable, and reserved built-in Agents.
- Prepared format v2 is a breaking clean cutover with no migration. Bundles
  installed by an older binary cannot decode; such a row is skipped with a
  named reinstall warning instead of wedging the runtime.
- The runtime semantic fingerprint is v2: it folds a digest over the compiled-in
  roster together with the installed-bundle catalog identity, so editing a
  built-in prompt (which requires a rebuild) still shows up in identity.
