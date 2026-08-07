# AgentBundle Authoring

Author and install a public `AgentBundle`. **A bundle defines exactly one agent.** Install one bundle per specialist agent; the bundle system is how you define subagents. Static-only Bundles remain process-free. A public executable Bundle may add one activation-scoped Bun Compat sidecar for Bundle-local tools, hooks, and event handlers; Harness remains the agent runtime.

Examples:

- Retained static example: [`examples/bundle.hya.md`](examples/bundle.hya.md)
- Transient Bun example: [`examples/bun-transient/`](examples/bun-transient/)
- Resident Bun example: [`examples/bun-resident/`](examples/bun-resident/)
- Working split-entrypoint example: [`docs/examples/bun-disjoint`](examples/bun-disjoint/) (`bun-disjoint`)

Built-in agents (`build`, `plan`, `explore`, `general`, the reserved `compaction` / `summary` / `title`, and the `hya-*` development agents) are **not** bundles. They are compiled into the binary in [`crates/hya-core/src/builtin_agents/`](../crates/hya-core/src/builtin_agents/) and run on the full Harness tool plane. Everything below describes installed bundles only.

## Runtime boundary

Harness is the sole agent, model, task, mailbox, event, `MemberOutcome`, and recovery runtime. The per-activation Bun Compat child supplies only Bundle-local tools, hooks, and event handlers. It never runs an agent/model loop and never receives a task, prompt, transcript, model state, grants, or runtime snapshot. There is no `agent/invoke`, sidecar send/wait, agent terminal/artifact result, second runtime, or second transport.

Harness's `SessionEngine` remains the only agent runtime. The sidecar never receives task/prompt/transcript or returns `MemberOutcome`; each executable activation owns one per-activation process.

Bun Compat is the sole executable sidecar implementation. Static-only Bundles remain process-free.

---

## Source layouts: `bundle.yaml` vs `bundle.hya.md`

A bundle **source directory** must contain **exactly one** of:

| File | Form |
| --- | --- |
| `bundle.yaml` | Plain YAML manifest (no embedded prompt body). Used by the shipped built-in bundles. |
| `bundle.hya.md` | YAML frontmatter fenced by `---` plus a markdown body. |

Rules ([`prepare.rs` `parse_source`](../crates/hya-bundle/src/prepare.rs)):

- **Both present** → hard error (`source contains both bundle.yaml and bundle.hya.md`).
- **Neither present** → `UnsupportedSource`.
- The manifest (YAML portion) is parsed with **`deny_unknown_fields`**: an unrecognised key is a hard error, not a warning.

### `bundle.hya.md` constraints

- Requires a leading `---` YAML frontmatter fence; missing frontmatter fails preparation.
- A **nonempty** markdown body becomes the agent's prompt. The agent must **not** also set `prompt:` to a file path.
- An **empty** body plus an explicit `prompt:` path is allowed: the body is "absent", not "an empty prompt", so the named file wins.

Because a bundle defines exactly one agent, the body is unambiguously that
agent's prompt — there is nothing to disambiguate.

The archive rules are: undeclared directory files are ignored; unreferenced archive files
are rejected.

### `bundle.yaml` constraints

- Plain YAML only (no markdown body).
- Required whenever the agent uses `prompt: path/to/file.md` instead of a body prompt.
- Real example: [`crates/hya-bundle/tests/fixtures/directory/bundle.yaml`](../crates/hya-bundle/tests/fixtures/directory/bundle.yaml).

A public archive contains the root manifest (`bundle.hya.md` or the prepared closure equivalent) plus exactly the normalized paths represented by the v1 agent prompt/resource/Extension contract, and nothing else. Undeclared directory files are ignored; unreferenced archive files are rejected. Missing declared files, wrapper directories, duplicate normalized paths, traversal, absolute paths, and non-regular files fail closed. Directory and archive forms of the same declared closure prepare to the same canonical identity and digest.

The retained static package can be created with:

```sh
7z a -t7z -mx=0 -ms=off static.hyabundle bundle.hya.md
```

A multi-file executable package references its closure explicitly:

```sh
7z a -t7z -mx=0 -ms=off bun.hyabundle bundle.hya.md extensions/runtime.js
```

The runtime uses a strict in-process reader and never shells to system `7z`.

### Package limits

Public package extraction enforces ([`package.rs`](../crates/hya-bundle/src/package.rs)):

| Limit | Value |
| --- | --- |
| Max archive size | **128 MiB** |
| Max per-entry (manifest entry) size | **64 MiB** |
| Max total expanded size | **256 MiB** |
| Max expansion ratio | **1000:1**, checked **streaming per chunk** (zip-bomb aborts mid-read) |
| Max path length | **1024** bytes |
| Max path depth | **32** segments |

---

## Manifest reference

Both markers are required at the top of the YAML:

```yaml
kind: AgentBundle
```

### Top-level keys

| Key | Required | Meaning |
| --- | --- | --- |
| `kind` | yes | Must be `AgentBundle`. |
| `identity` | yes | Bundle identity block (see below). |
| `resources` | no | `tools`, `skills`, `mcp`, `hooks` resource lists. |
| `extensions` | no | `js`, `rust` extension lists. |
| `agent` | yes | The single agent this bundle defines. |

**Removed keys.** `api_version` and per-agent `harness_access` no longer exist,
and `agents:` (a list) is replaced by `agent:` (a map). A manifest that still
carries any of them is rejected by name with `RemovedManifestKey`, which says
what to write instead.

**Unsupported in the current release** (declared but rejected at prepare):
`resources.mcp`, `extensions.rust`, and per-agent `resource_profile`.

### `identity`

Required object with **`deny_unknown_fields`**. Structural checks never establish publisher authenticity.

| Field | Rules |
| --- | --- |
| `id` | Required. Must contain at least one `/` and may use only `[A-Za-z0-9/-_.]`. Canonical form: `<publisher>/<name>` (e.g. `acme/reviewer`). |
| `version` | Required, non-empty after trim. |
| `publisher` | Required string. |

### `resources`

Each of `tools`, `skills`, `mcp`, and `hooks` is a list of:

| Field | Meaning |
| --- | --- |
| `id` | Local id inside the bundle. |
| `path` | File path inside the source tree (normalized, no traversal). |
| `aliases` | Optional extra bundle-local names usable in `resource_view`. |

On prepare, each resource gets:

- a SHA-256 **content digest** of the file bytes
- a **stable id** `bundle:<bundle_id>/<kind>/<id>` (kinds: `tool`, `skill`, `mcp`, `hook`; extensions use `extension`)

An alias that collides with an existing tool/skill **id** or another alias is an **`AliasCollision`** error.

**Skills example** (no shipped example currently includes one):

```yaml
resources:
  skills:
    - id: review-checklist
      path: skills/review-checklist.md
      aliases: [checklist]
  tools:
    - id: echo
      path: extensions/runtime.js
```

Filesystem `SKILL.md` discovery (outside bundles) is documented in
[Skills](skills.md).

### `extensions`

| Field | Meaning |
| --- | --- |
| `js` | JavaScript extension resources (same `{id, path, aliases}` shape). |
| `rust` | **Unsupported** — non-empty list fails prepare. |

### Per-agent fields

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Stable agent id: the public `AgentName` bytes (events, projection, resume) and the selector name. Also addressable as `bundle:<bundle_id>/agent/<id>`. |
| `description` | no | Human/model-facing text in selectors and spawn menus; omitting it leaves the agent unlabeled in pickers. |
| `role` | yes | `main` (TUI-selectable) or `subagent` (hidden from direct selector). Selector only — spawn uses `can_spawn`. |
| `color` | no | Optional display color on the prepared agent. |
| `prompt` | conditional | Path to prompt file, or omitted when `bundle.hya.md` body supplies the prompt. |
| `model_policy` | no | Optional `{ model, category, reasoning }` (all optional sub-fields; `deny_unknown_fields`). Per-agent model preference. |
| `workdir` | no | Optional working-directory string on the prepared agent. **Parsed and stored** on `PreparedAgent` and serialized into the prepared catalog. **Not applied** by the runtime today — no reader uses `PreparedAgent::workdir` to set session or tool workdirs; authors who set `workdir: subdir` get silent no-op behavior. |
| `spawn_lifecycle` | no | `transient` (default) or `resident`. |
| `resource_profile` | no | **Unsupported** if present — prepare fails. |
| `resource_view` | no | `allow`, `deny`, `aliases`, `namespace` (see below). |
| `can_spawn` | no | Allowlist of stable agent ids this agent may spawn. Targets are **not** resolved at prepare time — a bundle may name an agent from a bundle that is not installed yet. |
| `hook_refs` | no | Bundle-local hook resource references only (not `harness:hook/*`). |

Example agent with `model_policy`:

```yaml
agent:
  id: build
  description: Default coding agent
  role: main
  prompt: prompts/build.md
  model_policy:
    model: anthropic/claude-sonnet-4-6
    category: deep
    reasoning: high
  spawn_lifecycle: transient
  can_spawn: [explore, general]
```

---

## Tool plane

A bundle agent's tool plane is **derived from its origin, not declared**. There
is no manifest field that widens it.

| Origin | Tools | Skills | MCP |
| --- | --- | --- | --- |
| Built-in agent | the live registry snapshot, including MCP- and plugin-contributed tools | Harness skills, including project and user skills discovered in the workdir | Harness MCP exports |
| **Installed bundle agent** | the **internal public** tool snapshot captured when the runtime registry was built — never a tool contributed later by an MCP server or plugin | **none** from the Harness; only the bundle's own `resources.skills` | **none** from the Harness; only the bundle's own `resources.mcp` |

So an installed bundle agent sees exactly: the internal public tools, plus the
resources its own bundle ships. It cannot see a tool installed at the main-agent
level, a tool installed into hya directly, an MCP server's exports, or a project
or user skill.

Two consequences worth stating plainly:

- A `resource_view` entry naming `harness:skill/…` or `harness:mcp/…` fails with
  `ResourceNotInPlane`. That error means "outside your plane", not "misspelled".
- A `bundle:<other-bundle>/…` reference fails the same way. A bundle selects only
  its own resources.

**The clamp is not a sandbox.** `can_spawn` may name a built-in, and a spawned
built-in runs on *its* own full plane. The clamp bounds what a bundle agent does
directly, not what it can reach by delegating. Treat a bundle as a capability
declaration, not a security boundary.

**MCP is its own resource kind.** Even on the full plane, MCP exports are
**excluded** from the `tool` candidate partition (they are selected as `mcp`
references, not as `tool` references).

---

## `resource_view`

Deterministically narrows and renames the candidate set for one agent.

| Key | Meaning |
| --- | --- |
| `allow` | Reference list of candidates to include (sorted and deduped on prepare). |
| `deny` | Reference list removed after allow selection. |
| `aliases` | Map of public name → target reference for selected entries. |
| `namespace` | Optional segment used **only** for the **bundle-local** qualified public spelling `bundle:<namespace>/<kind>/<short>`. Default when omitted is the **bundle id**. Harness candidates keep their `harness:<kind>/<name>` qualified names; short public names are **never** prefixed. So an allow list of only `harness:tool/*` / `harness:skill/*` entries is unaffected by `namespace`. |

### Reference grammar

Each `allow` / `deny` entry resolves to a stable id. Accepted forms
([`resolve_global_reference`](../crates/hya-core/src/runtime_registry.rs)):

| Form | Example |
| --- | --- |
| Harness tool / skill / mcp | `harness:tool/read`, `harness:skill/foo`, `harness:mcp/server__tool` |
| Fully qualified bundle stable id | `bundle:<bundle_id>/tool/<local_id>` (also `…/skill/…`, `…/mcp/…`) |
| Bare bundle-local name or resource alias | `echo`, `checklist` |

**`allow` / `deny` kinds are only `tool`, `skill`, and `mcp`.** For both the
`harness:` and `bundle:` prefixes, `resolve_global_reference` hard-whitelists
those three kinds and returns `UnknownResourceReference` for `hook` or
`extension`. Do **not** put `bundle:<id>/hook/…` or `harness:extension/…` in
`allow` / `deny`.

Catalog **ExportKind** still includes five values (`tool`, `skill`, `mcp`,
`hook`, `extension`) for prepared resource indexing and stable ids of the form
`bundle:<bundle_id>/<kind>/<local_id>`. Hook selection for an agent uses the
separate `hook_refs` field only (Bundle-local hook resources). Extensions are
not referenceable through `resource_view`.

An **ambiguous bare name** (matches more than one candidate) is a
`NamespaceCollision` / resolution error. An **alias key** that collides with an
existing tool or skill public name is an **`AliasCollision`**.

### Hard failures

1. **Tool and MCP share one provider-facing namespace.** After public names are
   assigned, a tool name colliding with an MCP export’s public name is a
   **`NamespaceCollision`** that rejects the whole view. Disambiguate with
   `aliases` or `namespace`.
2. **Skill facade required.** Selecting any harness skill also requires selecting
   the `skill` tool facade (`harness:tool/skill`). Otherwise the view is rejected
   (`selected harness skills require the skill tool facade`), because skill bodies
   are only reachable through that tool.

### Example using deny and aliases

Harness-only views do not need `namespace` (it would be a no-op). Short aliases
and deny still apply:

```yaml
agent:
  id: explore
  role: subagent
  prompt: prompts/explore.md
  resource_view:
    allow:
      - harness:tool/write
    deny:
    aliases:
      search: harness:tool/grep
```

### Example: `namespace` on a bundle-local resource

When the view selects a **bundle-local** tool or skill, `namespace` rewrites only
that resource’s qualified public name (short name unchanged):

```yaml
# Bundle id is e.g. hya/docs-probe; a local skill local_id is "probe".
# With namespace: custom.ns the skill is also addressable as
# bundle:custom.ns/skill/probe (not bundle:hya/docs-probe/skill/probe).
agent:
  id: main
  role: main
  prompt: prompts/main.md
  resource_view:
    namespace: custom.ns
    allow:
      - harness:tool/skill
      - probe   # bare bundle-local skill id
```

---

## Stable identity, role, and spawn

`stable_id` is the public `AgentName`; preserve its bytes for events, projection, replay, fork, and resume. `local_id` is not a replacement public identity.

- `role: main` is selectable in the TUI direct selector.
- `role: subagent` is hidden from direct TUI selection.
- `role` controls selector visibility only. Agent-facing roster and ordinary spawn derive from the caller's `can_spawn` reachability, never from `role`.
- `spawn_lifecycle` is orthogonal to `role`.
- Empty or omitted `subagent_type` on the `task` tool normalizes to `general` before authorization.

### `can_spawn` enforcement

A `task` call resolves `subagent_type` against the turn’s bound agent definitions
([`authorize_spawn_target`](../crates/hya-app/src/runtime.rs)):

| Condition | Typed tool error |
| --- | --- |
| Id does not exist in the bound catalog | `UNKNOWN_AGENT_ID: <id>` (type `unknown_agent_id`) |
| Id exists but is outside the caller’s `can_spawn` allowlist | `AGENT_SPAWN_NOT_ALLOWED: <caller> cannot spawn <id>` (type `agent_spawn_not_allowed`) |

A `can_spawn` target that names an agent from a bundle that is not installed is
**skipped when the roster is built**, so one missing bundle never makes the
caller unusable; an actual spawn of that id still fails with
`UNKNOWN_AGENT_ID`.

Both surface to the model as **tool errors**, not permission prompts. Widening
`can_spawn` is the only fix; a permission rule cannot grant the spawn. Catalog
lookup does not rewrite an unknown `subagent_type` to `general`.

---

## Resources and permissions (Harness gate)

Bundle-local tool calls resolve against the activation's captured catalog binding. Existing `PermissionPlane` and plugin policy run before `tool/call`; denial prevents RPC. Host tools, static skills, and host-managed MCP remain governed by the Harness view. Bundle-declared MCP is unsupported. A Bundle adds no sandbox and causes no permission expansion.

---

## Selected Tool/Hook resources and the disjoint example

A compact executable source tree:

```text
bundle.hya.md
extensions/runtime.js
```

The working split-entrypoint example is [`docs/examples/bun-disjoint`](examples/bun-disjoint/) (`bun-disjoint`):

```text
docs/examples/bun-disjoint/
├── bundle.hya.md
├── prompts/
│   ├── alpha.md
│   ├── beta.md
│   └── static.md
└── extensions/
    ├── alpha.js
    └── beta.js
```

From `docs/examples/bun-disjoint/`:

```sh
7z a -t7z -mx=0 -ms=off bun-disjoint.hyabundle bundle.hya.md prompts/alpha.md prompts/beta.md prompts/static.md extensions/alpha.js extensions/beta.js
```

`hook_refs` select Bundle-local Hook resources only; all `harness:hook/*` spellings reject; supported hook IDs are exactly `event`, `tool.execute.before`, and `tool.execute.after`; aliases do not rename hooks. Harness host hooks stay outside AgentBundle metadata.

Each selected Tool or Hook source path must exact-path match exactly one JavaScript Extension resource in the referenced resource's owning bundle; cross-bundle selection joins in the owner. Never basename/prefix/digest/alias inference. The activation rule is: only selected Tool/Hook resources determine a deduplicated deterministic entrypoint list; staged does not mean activated.

Tool and Hook initialize declarations independently equal the selected expected sets regardless of order; missing, extra, duplicate, or unselected declarations reject. The contract is: tool-only reports zero hooks and hook-only reports zero tools. When authoring, generic superset modules are rejected and must be split; authors may instead select the complete set.

The public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists. Use external single-file bundling before packaging; activation never executes the authoring tree. Only selected captured PreparedResource bytes are rematerialized for activation. A missing relative helper import fails before ACK, with existing cleanup handling the failure before model or dispatch.

---

## Sidecar ABI and activation

The sidecar wire is newline-delimited JSON-RPC 2.0 using hya plugin protocol
version 1 (see [Plugin protocol](plugin-protocol.md)). Initialize remains
request/reply: initialize retains existing `protocol_version` and `host`
fields, and the only activation-specific metadata is
`{ activation_id, lifecycle }`. Activation begins only after initialize succeeds
and declarations match the prepared Bundle.

### Launch mechanics

1. Host creates **`<bundle-registry-parent>/activations/<activation_id>`**
   (registry parent from the installed-bundle path; fallback `activations/`).
2. Materializes each selected bundle tool/hook resource plus its unique
   exact-path-matching JS extension with `create_new`. Multi-owner activations
   use **`owner-0000/`-style** path slots (`owner-{index:04}`).
3. Spawns the Bun Compat adapter via `PluginClient::spawn_bundle` in that
   directory with **`env_clear()`**, appending  
   `-- --bundle-extension <absolute path>` once per resolved entrypoint.
4. Initialize reply must report **`protocol_version` 1** and plugin **`kind:
   compat`** or the child is terminated.

### `activation_id` validation

Rejected if empty, or if it contains `/`, `\`, `:`, or a NUL byte (or is not a
single normal path component). An activation id can never escape the staging
root.

stdout is protocol-only with a 1 MiB frame cap. stderr is diagnostic-only and
bounded. Initialization has a 5 second limit and request/reply has a 30 second
limit. A malformed response or timeout taints and terminates the sidecar.

---

## Lifecycle and recovery

- `spawn_lifecycle: transient` starts one child for the whole Harness activation and shuts it down and reaps it when the activation ends.
- `spawn_lifecycle: resident` reuses one healthy child across mailbox messages. Its in-process state is volatile; process loss never replays completed messages or effects.
- Idle resident loss lazily creates a fresh child and ACKs it under the same captured binding. Running loss aborts and fences the running item without replay, then preserves queued-after work for a fresh ACK under that binding.
- Explicit stop is final and idempotent.
- There is no TTL, heartbeat, idle reclaim, process adoption, watcher, or persisted PID/stdio/process state.

---

## Built-in agents are not bundles

Built-in agents are compiled into the binary as Rust constants in
[`crates/hya-core/src/builtin_agents/`](../crates/hya-core/src/builtin_agents/),
with prompt bodies pulled in by `include_str!`. Editing one requires a rebuild.

They differ from bundle agents in three ways:

- They run on the full Harness plane and own no bundle resources, so they never
  have a sidecar.
- Their ids are reserved: installing a bundle whose agent claims one is rejected
  at install with `BUNDLE_AGENT_ID_RESERVED`.
- An ordinary built-in spawns the **whole ordinary set**, so installing a bundle
  makes its agent spawnable immediately, with no edit to any built-in. The
  reserved system agents `compaction`, `summary`, and `title` are never
  selectable and never an ordinary spawn target.

---

## Prepared catalog format

Prepared catalogs use **`PREPARED_FORMAT_VERSION = 1`** and the document shape:

```text
{ format_version, bundles[], index[] }
```

**Canonical ordering** (non-canonical catalogs are rejected on decode):

- bundles sorted by id
- agents by `stable_id`
- resources by `local_id`
- `allow` / `deny` / `can_spawn` / `hook_refs` each strictly sorted

`PreparedCatalog::decode` verifies before anything loads:

- catalog SHA-256 against the expected digest
- rejection of non-canonical ordering
- recomputation of every per-resource and per-prompt content digest
- recomputation of each bundle digest (bundle JSON minus its own `digest` field)
- re-validation of all references
- rebuild-and-compare of the `index`

**Catalog semantic identity v1** is a domain-separated encoding over
`b"hya.bundle-catalog.semantic-identity/v1"` plus sorted per-catalog records of
`{ catalog digest, and each bundle’s id / version / publisher / digest }`.
Installing or removing any bundle changes the catalog identity. The runtime
fingerprint folds this together with a digest over the compiled-in built-in
roster, so editing a built-in prompt (which requires a rebuild) is visible in
identity too.

---

## Package and registry workflow

Use an exact lowercase `.hyabundle` suffix for the CLI commands:

```sh
hya bundle info -f bun.hyabundle
hya bundle install bun.hyabundle
hya bundle list
hya bundle info <bundle-id>
hya bundle uninstall <bundle-id>
```

`hya bundle info -f` inspects without mutating the registry or publication. Content magic, not the suffix, selects public/private parsing after the exact lowercase command suffix check. Installed generations publish atomically, and new root turns bind the new catalog while existing turns and children retain their pinned binding.

A bundle installed by an older binary cannot decode. Such a row is **skipped with a named warning** rather than wedging the runtime, and `hya bundle list` marks it `unreadable (reinstall)`. Reinstall it to restore the agent.

---

## Trust and unsupported combinations

Only public Bundles are supported for activation. Private inspection reports `authentication=unverified` and `payload=opaque`; private activation is unsupported and generation-preserving. Raw Rust extensions, Bundle-declared MCP, and resource profiles without an enforceable current host mapping are unsupported. Structural and declared-digest checks do not establish publisher authenticity. There is no sandbox and no permission expansion. Do not add decryption, signatures, a marketplace, compilation on activation, native commands, arbitrary environment access, a second permission plane, or legacy agent-file discovery. Legacy definitions are not parsed, migrated, or used as a fallback.
