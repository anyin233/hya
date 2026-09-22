<!-- Built-in skill; name and description are registered in skill_catalog.rs. -->

# Bundle authoring

An AgentBundle defines exactly one agent. Choose the existing payload matching the authored surface:

- `Plugin`: agentless tools, Skills, hooks, MCP, schemas, and optional process.
- `AgentBundle`: exactly one Agent and its private resource plane.
- `AgentSetBundle`: multiple Agents and optional restrictive `channels` templates; a channel-only package may omit Agents.
- `WorkflowBundle`: one Workflow and its exact reachable Agent closure.

Read `docs/agent-bundle-authoring.md`, `docs/bundle-runtime.md`, and the relevant
`docs/workflows.md`, `docs/agent-channels.md`, or `docs/claude-plugin-import.md`
page before changing an authoring contract. Preserve these names; do not invent
another bundle kind for tools, subagents, or channels.

## Runtime and trust

Harness owns Agent execution, model calls, admission, permission checks,
mailbox, events, projection, stop decisions, and recovery. A process extension
supplies resources through the native JSON-RPC plugin ABI; it does not become
a second Agent runtime. Evaluator hooks receive their documented evaluation
inputs while the engine retains iteration and budget caps.

`hya/core-agents`, `hya/base-tools`, and `hya/agent-channels` are trusted embedded
presets. Trust comes from the embedded source, never from a manifest claiming
their identity. Public bundle agents retain the internal-public Harness plane
plus selected resources from their own bundle. Ordinary core agents use the
Full plane, including shared Plugin exports. A resource view is not a sandbox:
a permitted spawn of another Agent runs that Agent on its own plane.

## Source and package closure

Use one root manifest. `bundle.hya.md` is the singular AgentBundle markdown
form; the other payloads use `bundle.yaml`. Declare every prompt, resource,
JavaScript extension, and inert support file. Missing files, path traversal,
absolute paths, duplicate normalized archive paths, unreferenced archive files,
and non-regular files reject. Directory and archive forms prepare to the same
canonical bytes and digest. Structural/digest validation is not publisher
verification.

`extensions.files` packages explicit UTF-8 support files; it does not activate
code by itself. `extensions.process` has `{kind: rust|bun|claude, command:
string[]}`. Native commands speak the plugin ABI, run from a private materialized
root, and may use `${BUNDLE_ROOT}`. No compilation or dependency installation
occurs during activation. Claude imports emit ordinary validated bundles.

`resources.mcp` entries point to validated MCP config JSON. Shared Plugin
exports use `<namespace>__mcp__<server>__<tool>`. A private Agent selects a server
resource and receives `<server-public-name>__<tool>`; MCP permission checks
remain intact. `schemas` bind URI schemes to declared owner tools.

Agent-bearing `extensions.js` use the existing activation-scoped Bun adapter.
Selected Tool/Hook files must exactly match declared JS entrypoints; initialization
must declare exactly the selected resources. These activation entrypoints remain
self-contained. An agentless JavaScript Plugin starts a shared generation-owned
Bun adapter without a synthetic Agent. Explicit process bundles instead declare
their complete process contribution set, then each Agent narrows it with its
resource view and `hook_refs`.

## Activation-scoped JavaScript contract

For agent-bearing JavaScript sidecars, `hook_refs` select Bundle-local Hook resources only;
all `harness:hook/*` spellings reject. Harness host hooks stay outside AgentBundle metadata.
The supported hook IDs are exactly `event`, `tool.execute.before`, and `tool.execute.after`;
aliases do not rename hooks. Process-backed bundles instead use the native hook vocabulary
and restrict calls by each Agent's selected references.

A selected Tool/Hook path must exact-path match a JavaScript Extension from its owning bundle.
For activation-scoped entrypoints, only selected Tool/Hook resources determine a deduplicated deterministic entrypoint list;
staged does not mean activated. Tool and Hook initialize declarations independently equal the selected expected sets regardless of order; missing, extra, duplicate, or unselected declarations reject.
A tool-only reports zero hooks and a hook-only reports zero tools. Selected Skill ids,
bytes, and digests also match prepared resources. Generic authoring rule:
generic superset modules are rejected and must be split, as in `bun-disjoint`.

The activation-scoped JS profile admits only self-contained selected Extension entrypoints; it does not load inert support files or discover transitive JS imports.
Use external single-file bundling; activation never executes the authoring tree.
A missing relative helper import fails before ACK. For source preparation,
undeclared directory files are ignored and unreferenced archive files are rejected.

The sidecar wire is newline-delimited JSON-RPC 2.0 using hya plugin protocol version 1.
During the handshake, initialize retains existing `protocol_version` and `host` fields; the only activation-specific metadata is `{ activation_id, lifecycle }`.
`tool/call` and `hook/*` use request/reply; `event` is a one-way notification.
Stdout is protocol-only and stderr is bounded diagnostics. The sidecar cannot
issue inbound Harness requests or create another Agent runtime.

## Identity, views, and lifecycle

Preserve public Agent ids byte-for-byte for events and replay. `role` controls
selector visibility; `spawn_lifecycle` independently chooses transient/resident.
An installed Agent's tool plane is **derived from its origin, not declared**.
**The clamp is not a sandbox.** A bundle Agent's `can_spawn` controls its roster; ordinary core Agents may spawn
ordinary catalog Agents. Unknown and unauthorized targets fail explicitly and
are never silently rewritten to `general`.

`resource_view.allow`, `deny`, `aliases`, and `namespace` narrow and name selected
resources. Private hooks run only when listed in `hook_refs`; shared Plugin hooks
join Full-plane Agents. Channel policy templates constrain existing unit or
parent-DM operations; runtime channel ids still come from engine events.

New process/MCP providers initialize before atomic publication. Failed startup
leaves the old generation intact. Running rounds keep their captured tools and
hooks after replacement or uninstall. Root turns may adopt updates at the next
model-round boundary; bound child and Workflow activations stay pinned. Unchanged sources are reused; private
materialized files remain as long as an old process owner is retained. Resident
mail/replay uses durable engine events, never persisted PIDs or stdio state.

## Package workflow

```sh
cargo run -p xtask -- package-bundle ./source ./example.hyabundle
hya-backend bundle info -f ./example.hyabundle
hya-backend bundle install ./example.hyabundle
hya-backend bundle list
hya-backend bundle info <bundle-id>
hya-backend bundle uninstall <bundle-id>
```

When building the minimal `bundle.hya.md` + one-entrypoint example manually,
enumerate regular files rather than directories:

```sh
7z a -t7z -mx=0 -ms=off example.hyabundle bundle.hya.md extensions/runtime.js
```

Public packages use the exact lowercase `.hyabundle` suffix. Inspection does
not mutate runtime state. Private activation, `extensions.rust` lists, and
resource profiles without an enforceable host mapping remain unsupported.
Bundle packages do not add a sandbox, another permission plane, or automatic
legacy agent-file discovery.
