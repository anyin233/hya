# Design — Bundle = one agent, isolated tool plane

Read `prd.md` first. This document gives the technical shape only.

## 1. The core split

Today one type, `PreparedAgent` inside a `PreparedBundle`, serves two different
things: a compiled-in system agent and a user-installed specialist. The design
splits them by origin and joins them again at one resolution seam.

```
                   +---------------------------+
                   |      AgentCatalog         |   (new, hya-core)
                   |  builtins + bundles       |
                   +------------+--------------+
                                |
              +-----------------+------------------+
              |                                    |
   +----------v-----------+          +-------------v--------------+
   |  BuiltinAgentRegistry|          |      BundleCatalog         |
   |  (hya-core, Rust)    |          |  (hya-bundle, installed)   |
   |  15 static defs      |          |  0..N single-agent bundles |
   |  plane = Full        |          |  plane = InternalPublic    |
   +----------------------+          +----------------------------+
```

One rule decides the tool plane: **origin decides**, never the manifest.

## 2. New and changed types

### 2.1 `hya-core::builtin_agents` (new module)

```rust
/// A compiled-in agent. Not a bundle, has no resources of its own.
pub struct BuiltinAgent {
    pub id: &'static str,
    pub description: Option<&'static str>,
    pub role: AgentRole,
    pub prompt: Option<&'static str>,
    pub model_policy: BuiltinModelPolicy,   // const-friendly ModelPolicy
    pub spawn_lifecycle: SpawnLifecycle,
    pub spawn_scope: SpawnScope,
    pub system_reserved: bool,              // compaction / summary / title
}

/// What a builtin may spawn. Evaluated against the live catalog.
pub enum SpawnScope {
    /// Every non-reserved agent in the catalog, builtin or installed.
    AllOrdinary,
    /// Spawns nothing.
    None,
}

pub const BUILTIN_AGENTS: &[BuiltinAgent] = &[ /* 15 entries */ ];
```

`SpawnScope::AllOrdinary` replaces the current hand-maintained `can_spawn`
anchor list in the two builtin YAML files. This is what satisfies R4.2: a newly
installed bundle agent becomes spawnable with no edit anywhere.

`compaction`, `summary`, and `title` get `system_reserved: true` and
`SpawnScope::None`. `AllOrdinary` filters reserved ids out, which preserves
today's `fixed_system_agents.rs` behaviour (R4.3).

Prompt bodies move from `bundles/builtin/*/prompts/*.md` to
`crates/hya-core/src/builtin_agents/prompts/*.md` and are pulled in with
`include_str!`. `build.rs` disappears; the compiler is the only build step.

Reasoning for `BuiltinModelPolicy`: `ModelPolicy` uses `Option<String>`, which
is not const-constructible. Use a parallel struct of `Option<&'static str>` with
a `to_model_policy()` conversion. Do not make `ModelPolicy` generic.

### 2.2 Shared definition view

Call sites must not branch on origin. Introduce one borrowed view:

```rust
pub enum AgentOrigin<'a> {
    Builtin,
    Bundle { bundle_id: &'a str },
}

pub struct AgentDefinition<'a> {
    pub stable_id: &'a str,
    pub description: Option<&'a str>,
    pub role: AgentRole,
    pub color: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub model_policy: Cow<'a, ModelPolicy>,
    pub workdir: Option<&'a str>,
    pub spawn_lifecycle: SpawnLifecycle,
    pub origin: AgentOrigin<'a>,
}
```

Every current `-> &PreparedAgent` signature becomes `-> AgentDefinition<'_>`.
Affected: `BundleCatalog::resolve_agent`, `resolve_agent_entry`, `resolve_spawn`,
`spawnable_agents`, and the `TurnBinding` wrappers around them.

`AgentRole`, `SpawnLifecycle`, and `ModelPolicy` stay in `hya-bundle` and are
re-exported from `hya-core`. Moving them to `hya-proto` is a larger refactor with
no benefit here; note it as follow-up debt, do not do it in this task.

### 2.3 `AgentCatalog` (new, `hya-core`)

```rust
pub struct AgentCatalog {
    builtins: &'static [BuiltinAgent],
    bundles: Arc<BundleCatalog>,
    builtin_digest: [u8; 32],
}
```

Responsibilities:

- `resolve(reference) -> Option<AgentDefinition<'_>>` — builtins first, then
  bundles, then `bundle:<id>/agent/<id>` form.
- `validate()` — reject an installed bundle whose agent id shadows a builtin id
  (R2.5). This runs at catalog construction, so a bad install fails at publish
  time, not at spawn time.
- `spawnable(caller) -> Vec<AgentDefinition<'_>>` — see 4.
- `builtin_digest()` — sha256 over the canonical serialisation of
  `BUILTIN_AGENTS`, computed once in a `OnceLock`. Feeds the runtime semantic
  fingerprint (R5.4) in place of the old builtin bundle digests.

`RuntimeSnapshot.catalog` changes from `Arc<BundleCatalog>` to
`Arc<AgentCatalog>`. `RuntimeRegistry::publish_catalog` takes `Arc<AgentCatalog>`.

### 2.4 `hya-bundle` model changes

```rust
pub struct PreparedBundle {
    pub format_version: u32,        // 2  (see PRD flagged assumption)
    pub identity: BundleIdentity,
    pub origin: BundleOrigin,       // Installed only now; keep the enum
    pub immutable: bool,
    pub digest: String,
    pub agent: PreparedAgent,       // was Vec<PreparedAgent>
    pub tools: Vec<PreparedResource>,
    pub skills: Vec<PreparedResource>,
    pub mcp: Vec<PreparedResource>,
    pub hooks: Vec<PreparedResource>,
    pub extensions: Vec<PreparedResource>,
}

pub struct PreparedAgent {
    pub id: AgentName,              // was local_id: String + stable_id: AgentName
    pub description: Option<String>,
    pub role: AgentRole,
    pub color: Option<String>,
    pub prompt: Option<String>,
    pub prompt_source: Option<String>,
    pub prompt_digest: Option<String>,
    pub model_policy: ModelPolicy,
    pub workdir: Option<String>,
    pub spawn_lifecycle: SpawnLifecycle,
    // harness_access: REMOVED
    pub resource_view: ResourceView,
    pub can_spawn: Vec<AgentName>,
    pub hook_refs: Vec<String>,
}

pub struct PreparedBundleIndex {
    pub bundle_id: String,
    pub version: String,
    pub digest: String,
    pub stable_agent_id: AgentName, // was Vec
}
```

`HarnessAccess` is deleted from `hya-bundle`. Its runtime replacement lives in
`hya-core` (see 3.1) and is never deserialised from a manifest.

Making `agent` a single field is what gives AC3 its teeth: "zero or two agents"
becomes unrepresentable, not merely rejected by a check.

### 2.5 Manifest shape

```yaml
kind: AgentBundle
identity:
  id: acme/reviewer
  version: 1.0.0
  publisher: acme
agent:
  id: acme-reviewer
  description: Reviews diffs for correctness and standards.
  role: subagent
  prompt: prompts/reviewer.md
  spawn_lifecycle: transient
  model_policy: { category: reasoning }
  resource_view:
    allow: [lint, bundle:acme/reviewer/skill/checklist]
  can_spawn: [explore]
  hook_refs: [pre-review]
resources:
  tools:  [{ id: lint, path: tools/lint.js }]
  skills: [{ id: checklist, path: skills/checklist/SKILL.md }]
  hooks:  [{ id: pre-review, path: hooks/pre.js }]
extensions:
  js: [{ id: main, path: ext/index.js }]
```

`SourceManifest` gains `#[serde(deny_unknown_fields)]` behaviour for the removed
keys. `deny_unknown_fields` alone gives an unhelpful serde error, so add explicit
`Option<serde::de::IgnoredAny>` capture fields for `api_version`, `agents`, and
`harness_access` and raise a targeted `BundleError::RemovedManifestKey { key,
guidance }` (R1.1, R1.3, R1.4). This is the difference between a usable error and
a support ticket.

`bundle.hya.md` keeps working. The current rule already requires exactly one
agent with no `prompt` field for the markdown form, so the markdown path
simplifies rather than changes.

## 3. The clamped tool plane

### 3.1 Plane selection

```rust
/// Which host-owned resources a bound agent may see. Derived from origin.
pub(crate) enum AgentToolPlane {
    /// Builtin agents. Live tool snapshot + harness skills + harness MCP.
    Full,
    /// Bundle agents. Builtin tool snapshot only. No harness skills or MCP.
    InternalPublic,
}
```

`AgentResourcePolicy` becomes:

```rust
pub struct AgentResourcePolicy {
    plane: AgentToolPlane,
    bundle: Option<String>,          // None for builtins
    resource_view: ResourceView,     // default for builtins
    selected_bundle_tool_ids: Arc<[String]>,
    canonical_hook_ids: Arc<[String]>,
}
```

`TurnBinding::agent_resource_policy` resolves the agent, reads its origin, and
sets the plane. There is no code path that reads a plane from a manifest.

### 3.2 Candidate collection

`collect_resource_candidates` changes to:

| Partition | `Full` (builtin) | `InternalPublic` (bundle) |
| --- | --- | --- |
| tool | live `snapshot.tools` | `snapshot.basic_tools` **only** |
| tool (bundle-local) | none — builtins own no bundle | all of `bundle.tools` |
| skill | all harness skills | none |
| skill (bundle-local) | none | all of `bundle.skills` |
| mcp | all harness MCP exports | none |
| mcp (bundle-local) | none | all of `bundle.mcp` |

`InternalPublic` matches today's `HarnessAccess::Basic` semantics exactly, so the
three `collect_harness_*` functions keep their bodies and only swap the gate
argument from `HarnessAccess` to `AgentToolPlane`. The behaviour change is that
the plane is no longer author-selectable, and `HarnessAccess::None` disappears.

Guard the builtin-collection calls behind `policy.bundle.is_some()`. Today they
call `catalog.bundle_resources(bundle_id, ..)` unconditionally and error when the
bundle id is unknown. A builtin agent has no bundle id, so an unguarded call
would hard-error on every builtin turn.

### 3.3 Escape-hatch closure (R3.5)

`resolve_global_reference` accepts `harness:skill/...` and `harness:mcp/...`. For
a bundle agent those candidates are absent from the partition, so resolution
already fails with `UnknownResourceReference`. That is the required fail-closed
behaviour, but it is currently incidental. Make it explicit: in
`resolve_global_reference`, when the plane is `InternalPublic` and the reference
kind is `skill` or `mcp` under the `harness:` prefix, return a dedicated
`BundleError::ResourceNotInPlane { reference, plane }`. The error must say why,
not just "unknown".

Also close the `allow`-driven side door in `collect_resource_candidates`: the
loop at `runtime_registry.rs:761` re-resolves `bundle:` tool references through
`catalog.resolve_resource_entry`, which searches **all** bundles, not the caller's
bundle. Restrict that lookup to the caller's own bundle id. Without this, bundle
A can name `bundle:B/tool/x` in `allow` and pull in bundle B's tool. This is a
cross-bundle leak that exists today and that D3 forbids.

### 3.4 `basic_tools` invariant

`basic_tools` is captured in `RuntimeRegistry::from_snapshot` and is only ever
cloned forward, never extended. In production it is `ToolRegistry::builtins()`
minus `websearch` when config disables it (`hya-app/src/runtime.rs:3968`). MCP
and plugin tools are published later through `refresh`, so they never reach it.

That is a convention held by one comment. Turn it into an invariant:

- Add a debug assertion in `publish_candidate` that the published
  `basic_tools` is pointer- or content-equal to the current one.
- Add a test that registers an MCP source and a plugin source, then asserts the
  `InternalPublic` plane for a bundle agent is unchanged.

The `websearch` removal is a subtraction from the plane, so it stays fail-safe.
Record it in the docs so nobody treats the plane as a fixed constant list.

## 4. Spawn graph

`AgentCatalog::spawnable(caller)`:

1. Resolve `caller`. If it is a builtin, read its `SpawnScope`.
   - `AllOrdinary` -> every builtin with `system_reserved == false`, plus every
     installed bundle agent. Sorted by id for determinism.
   - `None` -> empty.
2. If the caller is a bundle agent, walk its `can_spawn` list and resolve each id
   against the whole `AgentCatalog`. **Skip** ids that do not resolve (R4.4).

`AgentCatalog::resolve_spawn(caller, requested)` stays strict: an unresolvable or
unlisted target is an error. So a missing target degrades the roster quietly but
never lets an unauthorised spawn through.

Rationale for the split: bundles are installed independently. If bundle A lists
`b-agent` from bundle B and B is not installed, today's `spawnable_agents` returns
`Err` for the whole roster and agent A becomes unusable. Skipping in the roster
and failing on the attempt keeps A working and still fails closed.

`prepare.rs` stops validating `can_spawn` targets (R4.5). A single-agent bundle
has no local set to validate against. Delete the check near
`crates/hya-bundle/src/prepare.rs:203`, and delete the rewrite loop near line 353
that maps local ids to stable ids — with one `id` field there is nothing to map.

## 5. Storage, registry, and CLI

### 5.1 Installed-bundle registry

`InstalledBundleRefresh::refresh_if_changed` already requires exactly one bundle
per prepared record. It now also requires exactly one agent, which the type
guarantees.

Old rows cannot decode. Today one bad row returns `Err` from
`refresh_if_changed`, which is called at every root binding, so every later turn
re-hits the failure (R5.2). Change to:

- On decode failure of a row, record a structured warning naming the bundle id
  and the reinstall action, **skip that row**, and continue with the rest.
- Advance `applied_generation` anyway, so the failure is reported once per
  generation and not on every turn.
- Add a `hya bundle doctor`-style line to `hya bundle list` output marking the
  row as `unreadable (reinstall)`.

Do not silently delete rows. The operator decides.

Bump the SQLite schema version so the migration path is explicit and a downgrade
is detectable.

### 5.2 Removals in `hya-store`

`is_immutable_builtin(builtins: &[PreparedBundle], bundle_id)` at
`crates/hya-store/src/bundle_registry.rs:450` loses its input. Builtins are no
longer bundles, so no builtin bundle id exists. Replace the immutability check
with the builtin **agent id** shadow check from `AgentCatalog::validate()`, and
reject the install at `hya bundle install` time with a clear message rather than
at catalog publish time.

### 5.3 CLI

- `crates/hya-backend/src/bundle_cmd.rs`: drop `print_builtin_info`; `list` and
  `info` show one agent per bundle and no builtin bundles.
- `crates/hya-backend/src/agent_cmd.rs`: list builtins and installed bundle
  agents together with an `origin` column (`builtin` / `bundle:<id>`).
- `crates/hya-app/src/runtime.rs`: delete `builtin_catalog()`,
  `builtin_catalog_from()`, `BUILTIN_BUNDLES`, `BUILTIN_BUNDLES_DIGEST`.

## 6. Semantic fingerprint

`runtime_source_dispatch_identity` and the runtime semantic fingerprint currently
fold in `BundleCatalog::semantic_identity_v1()`, which is derived from the
verified prepared-catalog records — builtins included.

New composition:

```
identity = domain_tag_v2
        || builtin_digest            (sha256 over canonical BUILTIN_AGENTS)
        || bundle_semantic_identity  (unchanged, installed bundles only)
        || tool_view || skill_view || source_view
```

Bump the domain tag to `hya.core.runtime-semantic-fingerprint/v2`. The builtin
digest is constant per binary, so a rebuilt binary with edited builtin prompts
produces a different fingerprint. That preserves the current property that
editing a builtin requires a rebuild and is visible in identity.

`BundleCatalog::from_prepared` must accept an empty slice (R2.4). Delete the
`EmptyPreparedCatalog` rejection. `from_verified_catalogs` with zero catalogs
yields an empty-but-valid identity.

## 7. Blast radius

| Crate / file | Change |
| --- | --- |
| `hya-bundle/src/model.rs` | `agent` singular, `id` collapse, drop `HarnessAccess`, index singular |
| `hya-bundle/src/source.rs` | `agent:` map, removed-key capture fields |
| `hya-bundle/src/prepare.rs` | single-agent prepare, drop `can_spawn` validation and id rewrite, drop `prepare_builtins` |
| `hya-bundle/src/catalog.rs` | allow empty, single-agent indexing, drop builtin origin paths |
| `hya-bundle/src/error.rs` | `RemovedManifestKey`, `ResourceNotInPlane`, drop `EmptyPreparedCatalog` |
| `hya-bundle/src/package.rs` | inspection reports one agent |
| `hya-core/src/builtin_agents/` | **new** — 15 defs + prompts |
| `hya-core/src/agent_catalog.rs` | **new** — union resolver, validate, spawnable, digest |
| `hya-core/src/runtime_registry.rs` | plane enum, policy rework, candidate gating, fingerprint v2 |
| `hya-core/src/engine.rs` | `AgentDefinition` instead of `&PreparedAgent` |
| `hya-app/build.rs` | **deleted** |
| `hya-app/src/runtime.rs` | drop `builtin_catalog*`, build `AgentCatalog` |
| `hya-app/src/installed_bundle_refresh.rs` | skip-and-warn on bad rows |
| `hya-store/src/bundle_registry.rs` | schema bump, builtin-agent-id shadow reject |
| `hya-backend/src/bundle_cmd.rs`, `agent_cmd.rs` | output shape |
| `bundles/builtin/` | **deleted** |

Test surfaces that must be rewritten, not merely fixed: `hya-bundle/tests/*`
(9 files), `hya-core/tests/{runtime_registry, agent_resource_view,
role_selector_vs_can_spawn_roster, fixed_system_agents, subagent,
runtime_catalog_refresh, root_turn_bundle_precedence, historical_agent_identity,
runtime_generation}.rs`, `hya-app/tests/{builtin_bundle_build,
installed_bundle_refresh, nested_spawn_tree, spawn_admission}.rs`,
`hya-backend/tests/bundle_cli.rs`, `hya-server/src/compat/reference_tests.rs`.

The five `.7z` fixtures under `hya-bundle/tests/fixtures/packages/` must be
regenerated from new-format sources. Add an `xtask` command that rebuilds them
from a checked-in source tree, so they stop being opaque binaries.

## 8. Tradeoffs and risks

**Accepted: a bundle main agent can spawn a full-plane builtin.** D6 allows
`can_spawn: [general]`, and `general` runs with the full plane. So the clamp
limits what a bundle agent does *directly*, not what it can reach through
delegation. This is a deliberate usability choice. Document it plainly in the
authoring guide so nobody mistakes the clamp for a sandbox.

**Risk: the `id` collapse changes stable ids.** Today `local_id` and `stable_id`
can differ. Every builtin has them equal, and the docs examples use a distinct
pair. Collapsing to one field means an existing bundle whose two ids differ
changes its stable id. Since D7 drops v1 outright and bundles must be reinstalled,
this is contained — but call it out in the release note.

**Risk: `AgentDefinition<'a>` borrow lifetimes.** `TurnBinding` holds
`Arc<RuntimeSnapshot>`, so borrows are tied to the binding, which is what current
`&PreparedAgent` returns already do. Expect friction where a definition is held
across an `await`. If it appears, return an owned `AgentDefinitionOwned` from the
few async call sites rather than fighting the borrow checker in the hot path.

**Risk: fingerprint churn.** Bumping the domain tag invalidates every persisted
fingerprint. Confirm that no stored session replays compare fingerprints across
the upgrade boundary; `historical_agent_identity.rs` is the test that will tell.

## 9. Rollout and rollback

Rollout is a single release. There is no dual-format window, by D7.

- Release note states: every installed bundle must be reinstalled after upgrade.
- `hya bundle list` marks unreadable rows so the operator sees the work needed.
- Built-in agents keep their ids, so existing sessions bound to `build`, `plan`,
  or any `hya-*` agent keep resolving.

Rollback is a binary revert. The installed-bundle registry rows written by the
new version are unreadable by the old one, in the same skip-and-warn way, so a
revert degrades to "reinstall old-format bundles" rather than a hard failure.
That symmetry is the reason for the skip-and-warn behaviour in 5.1.
