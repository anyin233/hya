# One agent per AgentBundle, with a host-controlled tool plane

An `AgentBundle` defines **exactly one** agent, and that agent's tool plane is
derived from where the agent came from rather than declared in its manifest.

Two things forced this. A bundle used to be both "a package of agents" and "an
agent definition", and the two roles pulled the format in opposite directions.
And an installed bundle could declare `harness_access: full`, which handed the
bundle author — not the host — the decision about how much of the machine the
agent could see.

## Decision

**A bundle is one agent.** `PreparedBundle.agent` is a single field, so a bundle
with zero or two agents is unrepresentable rather than merely rejected. Install
one bundle per specialist agent; bundles are how subagents are defined.

**Built-in agents leave the bundle system.** The 15 built-ins are Rust constants
in `crates/hya-core/src/builtin_agents/`, with prompts pulled in by
`include_str!`. `bundles/builtin/`, `hya-app/build.rs`, `prepare_builtins`,
`BundleOrigin`, and the `immutable` flag are gone. `AgentCatalog` joins the
compiled-in roster with the installed `BundleCatalog` behind one resolution
seam, so call sites read an origin off an `AgentDefinition` instead of branching
on "is this a bundle".

**Origin decides the tool plane.** `AgentToolPlane::for_origin` is the only
producer:

| Origin | Plane |
| --- | --- |
| Built-in | `Full` — the live tool snapshot, Harness skills, Harness MCP |
| Installed bundle | `InternalPublic` — the builtin tool snapshot captured at registry construction, plus the bundle's own resources, and nothing else |

`harness_access` and `api_version` are deleted from the manifest, and `agents:`
becomes `agent:`. A manifest that still carries any of them is rejected by name
so the author is told what to write instead.

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
- `can_spawn` is no longer resolved at prepare time — a single-agent bundle
  cannot see a cross-bundle target. A missing target is skipped when the roster
  is built and refused at the actual spawn, so one uninstalled bundle never
  makes another agent unusable.
- The format change is breaking with no migration. Bundles installed by an older
  binary cannot decode; such a row is skipped with a named warning instead of
  wedging the runtime, and `hya bundle list` marks it `unreadable (reinstall)`.
- The runtime semantic fingerprint is v2: it folds a digest over the compiled-in
  roster together with the installed-bundle catalog identity, so editing a
  built-in prompt (which requires a rebuild) still shows up in identity.
