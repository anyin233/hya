# Core Agents Preset

## Introduction

`hya/core-agents` is the trusted, immutable `AgentSetBundle` that supplies the
agents shipped with hya. Its source lives under `bundles/presets/core-agents`.
It is a [first-party bundle](bundle-runtime.md#first-party-bundles):
`hya_bundle::first_party_bundle("hya/core-agents")` validates and prepares
that source once per process, and the runtime catalog resolves built-in
agents from those verified, loaded bytes. In a Cargo build the in-tree source
directory is read at startup, so edits apply on the next restart with no
rebuild; an installed backend instead loads the packaged
`hya-core-agents.hyabundle` beside it.

The preset preserves the stable agent ids, prompts, selector roles, model
defaults, and reserved system-agent behavior that
existing sessions expect. The trusted preset origin selects the full host tool
plane. Public `AgentBundle` and `AgentSetBundle` manifests cannot request that
origin or the full plane, and an installed bundle cannot claim any core agent
id.

## Usage

Core agents require no installation or configuration. Select an ordinary agent
by its stable id, such as `build`, `plan`, or `hya-main`. Code that needs an
explicit bundle-qualified reference may use
`bundle:hya/core-agents/agent/build`.

For example, resolving `build` and
`bundle:hya/core-agents/agent/build` through `AgentCatalog` returns the same
definition. Ordinary core agents can spawn every ordinary catalog agent,
including agents from installed bundles. `compaction`, `summary`, and `title`
are reserved for engine-owned operations: they are resolvable by exact id but
are excluded from selectors and spawn rosters and cannot spawn other agents.

Editing `bundle.yaml` or a prompt under the preset directory takes effect on
the next restart; no rebuild is required in a Cargo build. Invalid source
fails at startup, before the runtime catalog can load it.

The application exposes `hya/core-agents`, the five trusted
[tool-family presets](base-tools.md), the trusted
[core Skills](skills.md#built-in-fallback-skills), `hya/core-commands`, and the
channel defaults in `hya/agent-channels` through a read-only preset inventory
(nine presets in total) for list/info surfaces. Inventory rows report
their id, kind, version, digest, exported ids, and
`immutable: true, installable: false`. They do not enter the installed bundle
catalog and cannot be upgraded or uninstalled through public bundle commands.

## Interface definitions

The source payload is an `AgentSetBundle` with identity `hya/core-agents` and
publisher `hya`. Every member uses the standard Agent interface: `id`, optional
`description`, `role`, optional `prompt`, `model_policy`, `workdir`,
`resource_view`, `can_spawn`, and `hook_refs`. The shipped
agents leave `model_policy` empty so the runtime's configured model remains the
default. When spawned with `task`, every agent (built-in or bundle) runs as a
resident actor; `spawn_lifecycle` is a removed manifest key. The preset's inert `policy.yaml`
explicitly names engine-only reserved ids and the ordinary spawn scope; loading
the preset validates every policy id against a real prepared Agent. This policy is
specific to the trusted preset origin and does not grant special behavior to
public bundles.

`hya_core::core_agents_preset()` loads and verifies the bundle once per
process and returns a `CoreAgentsPreset` view (or the load error):

- `bundle_id() -> &'static str` returns `hya/core-agents`.
- `prepared_bytes() -> &'static [u8]` returns the loaded canonical document.
- `digest() -> &'static str` returns its 64-character SHA-256 hex digest.
- `agents() -> &'static [PreparedAgent]` returns agents in stable-id order.

`hya_core::builtin_agents() -> &'static [BuiltinAgent]` (replacing the old
`BUILTIN_AGENTS` constant) returns the same roster as a compatibility view for
callers that do not need the full preset.

`AgentOrigin::Builtin` is the compatibility spelling for this trusted preset
origin. `is_preset()` identifies it and `preset_bundle_id()` returns
`Some("hya/core-agents")`. `bundle_id()` remains reserved for installed public
bundle ownership, preserving existing model-preference and resource lookup
behavior.
