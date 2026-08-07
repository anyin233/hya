# AgentBundle Authoring (0.34.11)

Author and install a public `AgentBundle`. Static-only Bundles remain process-free. A public executable Bundle may add one activation-scoped Bun Compat sidecar for Bundle-local tools, hooks, and event handlers; Harness remains the agent runtime.

Examples:

- Retained static example: [`examples/bundle.hya.md`](examples/bundle.hya.md)
- Transient Bun example: [`examples/bun-transient/`](examples/bun-transient/)
- Resident Bun example: [`examples/bun-resident/`](examples/bun-resident/)
- Working split-entrypoint example: [`docs/examples/bun-disjoint`](examples/bun-disjoint/) (`bun-disjoint`)
- Shipped built-in directory form: [`bundles/builtin/hya-core-agents/bundle.yaml`](../bundles/builtin/hya-core-agents/bundle.yaml)

## Runtime boundary

Harness is the sole agent, model, task, mailbox, event, `MemberOutcome`, and recovery runtime. The per-activation Bun Compat child supplies only Bundle-local tools, hooks, and event handlers. It never runs an agent/model loop and never receives a task, prompt, transcript, model state, grants, or runtime snapshot. There is no `agent/invoke`, sidecar send/wait, agent terminal/artifact result, second runtime, or second transport.

Harness's `SessionEngine` remains the only agent runtime. The sidecar never receives task/prompt/transcript or returns `MemberOutcome`; each executable activation owns one per-activation process.

Bun Compat is the sole executable sidecar implementation supported in 0.34.11. Static-only Bundles remain process-free.

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
- A **nonempty** markdown body becomes the prompt for **exactly one** agent; that agent must **not** also set `prompt:` to a file path.
- An **empty** body plus an explicit per-agent `prompt:` path is allowed for multi-agent sources only when **every** agent names a prompt file (then the body is not used as a prompt).
- If the body is nonempty, the source must have exactly one agent without a `prompt` path.

An empty Markdown body plus explicit per-agent `prompt:` paths enables multiple agents;
every agent in that form needs an explicit prompt path.

The archive rules are: undeclared directory files are ignored; unreferenced archive files
are rejected.

### `bundle.yaml` constraints

- Plain YAML only (no markdown body).
- Required whenever agents use `prompt: path/to/file.md` (or other multi-agent layouts that do not carry a body prompt).
- Real example: [`bundles/builtin/hya-core-agents/bundle.yaml`](../bundles/builtin/hya-core-agents/bundle.yaml).

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
api_version: hya.agent-bundle/v1
kind: AgentBundle
```

### Top-level keys

| Key | Required | Meaning |
| --- | --- | --- |
| `api_version` | yes | Must be `hya.agent-bundle/v1`. |
| `kind` | yes | Must be `AgentBundle`. |
| `identity` | yes | Bundle identity block (see below). |
| `resources` | no | `tools`, `skills`, `mcp`, `hooks` resource lists. |
| `extensions` | no | `js`, `rust` extension lists. |
| `agents` | yes | Non-empty list of agent definitions. |

**Unsupported in the current release** (declared but rejected at prepare):
`resources.mcp`, `extensions.rust`, and per-agent `resource_profile`.

### `identity`

Required object with **`deny_unknown_fields`**. Structural checks never establish publisher authenticity.

| Field | Rules |
| --- | --- |
| `id` | Required. Must contain at least one `/` and may use only `[A-Za-z0-9/-_.]`. Canonical form: `<publisher>/<name>` (e.g. `hya/core-agents`). |
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
| `local_id` | yes | Bundle-local agent id. |
| `stable_id` | yes | Public `AgentName` bytes (events, projection, resume). |
| `description` | no | Human/model-facing text in selectors and spawn menus; omitting it leaves the agent unlabeled in pickers. |
| `role` | yes | `main` (TUI-selectable) or `subagent` (hidden from direct selector). Selector only — spawn uses `can_spawn`. |
| `color` | no | Optional display color on the prepared agent. |
| `prompt` | conditional | Path to prompt file, or omitted when `bundle.hya.md` body supplies the prompt. |
| `model_policy` | no | Optional `{ model, category, reasoning }` (all optional sub-fields; `deny_unknown_fields`). Per-agent model preference. |
| `workdir` | no | Optional working-directory hint on the prepared agent. |
| `spawn_lifecycle` | no | `transient` (default) or `resident`. |
| `resource_profile` | no | **Unsupported** if present — prepare fails. |
| `harness_access` | yes | `none` \| `basic` \| `full` (see below). |
| `resource_view` | no | `allow`, `deny`, `aliases`, `namespace` (see below). |
| `can_spawn` | no | Allowlist of stable agent ids this agent may spawn. |
| `hook_refs` | no | Bundle-local hook resource references only (not `harness:hook/*`). |

Example agent with `model_policy`:

```yaml
agents:
  - local_id: build
    stable_id: build
    description: Default coding agent
    role: main
    prompt: prompts/build.md
    model_policy:
      model: anthropic/claude-sonnet-4-6
      category: deep
      reasoning: high
    spawn_lifecycle: transient
    harness_access: full
    can_spawn: [explore, general]
```

---

## `harness_access`

Chooses the Harness-owned candidate tool set before `resource_view` narrows it
([`collect_harness_tool_candidates`](../crates/hya-core/src/runtime_registry.rs)):

| Value | Exposure |
| --- | --- |
| `none` | No Harness tools. The agent sees only Bundle-local tools (and other non-tool resources the view selects). |
| `basic` | Only the **original builtin tool snapshot** captured when the runtime registry was constructed — **not** tools later contributed by MCP servers or plugins. |
| `full` | Builtins plus MCP- and plugin-contributed tools present in the live registry snapshot. |

**MCP is its own resource kind.** Even under `full`, MCP exports are **excluded**
from the `tool` candidate partition (they are selected as `mcp` references, not
as `tool` references).

---

## `resource_view`

Deterministically narrows and renames the candidate set for one agent.

| Key | Meaning |
| --- | --- |
| `allow` | Reference list of candidates to include (sorted and deduped on prepare). |
| `deny` | Reference list removed after allow selection. |
| `aliases` | Map of public name → target reference for selected entries. |
| `namespace` | Optional prefix for public names; default is the bundle id. |

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

### Example using deny, aliases, and namespace

```yaml
agents:
  - local_id: explorer
    stable_id: explore
    role: subagent
    prompt: prompts/explore.md
    harness_access: full
    resource_view:
      namespace: explore
      allow:
        - harness:tool/read
        - harness:tool/grep
        - harness:tool/glob
        - harness:tool/skill
        - harness:skill/customize-compat
      deny:
        - harness:tool/write
      aliases:
        search: harness:tool/grep
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

The 0.34.11 public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists. Use external single-file bundling before packaging; activation never executes the authoring tree. Only selected captured PreparedResource bytes are rematerialized for activation. A missing relative helper import fails before ACK, with existing cleanup handling the failure before model or dispatch.

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

## Built-in bundles

Two bundles are prepared at **compile time** and embedded into `hya-app`
([`crates/hya-app/build.rs`](../crates/hya-app/build.rs)):

### `hya/core-agents`

Source: [`bundles/builtin/hya-core-agents/`](../bundles/builtin/hya-core-agents/).

| Agent | Role (selector) | Notes |
| --- | --- | --- |
| `build` | main | Default agent |
| `plan` | main | Plan mode |
| `explore` | subagent | Codebase exploration |
| `general` | subagent | Multi-step general work |
| `compaction` | subagent | Prompt under `prompts/` |
| `summary` | subagent | Prompt under `prompts/` |
| `title` | subagent | Prompt under `prompts/` |

Ordinary agents share a `can_spawn` anchor listing all ordinary agent ids
(including `hya/*` development agents).

### `hya/development`

Source: [`bundles/builtin/hya-development/`](../bundles/builtin/hya-development/).

| Agent |
| --- |
| `hya-main` (main) |
| `hya-planner`, `hya-implementer`, `hya-reviewer`, `hya-tester`, `hya-docs`, `hya-explorer`, `hya-release` (subagents) |

Prompts live under `prompts/`.

### Embedding and fail-closed load

- `build.rs` prepares both source dirs into `OUT_DIR/builtin-bundles.json` plus a
  `.sha256` digest, with `cargo:rerun-if-changed` on both trees — **editing a
  builtin requires a rebuild**, not a process restart.
- `builtin_catalog()` decodes and validates the embedded artifact **exactly once**
  in a `OnceLock` and caches **both success and failure**. A tampered or invalid
  artifact leaves the process **without any builtin agents** for its whole
  lifetime (no silent retry).

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
`{ catalog digest, and each bundle’s id / version / publisher / origin /
immutable / digest }`. Installing or removing any bundle changes the catalog
identity.

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

`hya bundle info -f` inspects without mutating the registry or publication. Content magic, not the suffix, selects public/private parsing after the exact lowercase command suffix check. Installed generations publish atomically, and new root turns bind the new catalog while existing turns and children retain their pinned binding. Built-ins are merged read-only and cannot be replaced or uninstalled.

---

## Trust and unsupported combinations

Only public Bundles are supported for activation in 0.34.11. Private inspection reports `authentication=unverified` and `payload=opaque`; private activation is unsupported and generation-preserving. Raw Rust extensions, Bundle-declared MCP, and resource profiles without an enforceable current host mapping are unsupported. Structural and declared-digest checks do not establish publisher authenticity. There is no sandbox and no permission expansion. Do not add decryption, signatures, a marketplace, compilation on activation, native commands, arbitrary environment access, a second permission plane, or legacy agent-file discovery. Legacy definitions are not parsed, migrated, or used as a fallback.
