# Creating Bundles (`.hyabundle`)

A **Bundle** (`kind: Bundle`) is Hya's all-in-one package format. A single `.hyabundle` file (or project folder under `.hya/bundles/`) can ship:

- **Skills, Tools, MCP servers, and Hooks** for existing agents (*agentless Plugin role*),
- **One or more custom Agents** with scoped permissions and channel policies,
- **A compiled Workflow** with its stage agents,
- **Custom HTTP endpoints** (`apis:`), **custom permission modes** (`permission_modes:`), **URI schemes** (`schemas:`), and **TUI panels** (`tui:`).

Static bundles run zero extra processes. Executable bundles run either out-of-process **JS Bundles** (Bun / Claude sidecars over stdio NDJSON JSON-RPC 2.0) cleanly isolated from Hya's core engine, or **Native Bundles** (compiled Rust `cdylib` libraries speaking Lockstep Native ABI v2 via `crates/hya-runtime-abi`, optionally paired with `@hya/tui-sdk` v2 TUI extensions in a hybrid `native` + `tui` bundle).

For the complete normative format, all five descriptor file schemas, loading precedence, and independent CI architecture, see the **[`hyabundle` Specification](hyabundle-spec.md)**, **[Native Bundle Runtime ABI v2](native-abi.md)**, and **[ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)**.

---

## 1. Your first Bundle in 60 seconds

### Single-file Agent Bundle (`bundle.hya.md`)

For a simple custom agent that uses Hya's built-in tools, you only need **one file** named `bundle.hya.md` (see [`docs/examples/bundle.hya.md`](../examples/bundle.hya.md)):

```markdown
---
kind: Bundle
schema_version: 2
identity:
  id: acme/sql-helper
  version: 1.0.0
  publisher: acme
agents:
  - id: sql-helper
    description: Reviews SQL queries and schema migrations safely.
    role: main
    resource_view:
      allow:
        - harness:tool/read
        - harness:tool/grep
        - harness:tool/glob
---
You are a PostgreSQL specialist. Inspect queries and migration files carefully, flag lock hazards or missing indexes, and explain fixes clearly.
```

Package, verify, and install it:

```sh
# Package the folder containing bundle.hya.md into a deterministic .hyabundle
hya bundle pack ./sql-helper sql-helper.hyabundle

# Dry-run verification (supports both directories and .hyabundle archives)
hya bundle verify --strict ./sql-helper
hya bundle verify --strict --protocol-smoke sql-helper.hyabundle

# Install globally (or pass --project to install into ./.hya/bundles/)
hya bundle install -y sql-helper.hyabundle

# Use it right away inside Hya with `/agent sql-helper`!
hya bundle list
```

### Multi-file Directory Bundle (`bundle.yaml`)

When packaging multiple agents, a Workflow, skills, or JS/TS extensions, use `bundle.yaml` instead of `bundle.hya.md` (a folder must contain **either** `bundle.yaml` **or** `bundle.hya.md`, never both):

```yaml
kind: Bundle
schema_version: 2
identity:
  id: acme/review-pack
  version: 1.0.0
  publisher: acme
check:
  command: ["bun", "test", "--pass-with-no-tests"]   # Optional self-test run automatically on verify/install!
  timeout_secs: 120
resources:
  skills:
    - id: review-checklist
      path: skills/review-checklist/SKILL.md
agents:
  - id: acme-reviewer
    description: Security & correctness code reviewer
    role: subagent
    prompt: prompts/reviewer.md
    model_policy:
      category: deep
      reasoning: high
    resource_view:
      allow:
        - harness:tool/read
        - harness:tool/grep
        - harness:tool/skill
        - review-checklist
```

Check out runnable source examples in [`docs/examples/`](../examples/):
- [`docs/examples/bundle.hya.md`](../examples/bundle.hya.md) — Single-file static agent
- [`docs/examples/bun-transient/`](../examples/bun-transient/) & [`docs/examples/bun-resident/`](../examples/bun-resident/) — Agent + Bun JS tool
- [`docs/examples/bun-disjoint/`](../examples/bun-disjoint/) — Multi-agent bundle selecting disjoint JS tools and hooks

---

## 2. Manifest Reference (`bundle.yaml`)

Hya v2 accepts **`schema_version: 2` only** (upgrade v1 manifests with `hya-migration run`). Unknown YAML fields are rejected (`deny_unknown_fields`), so typos are caught at package time.

| Top-level key | Purpose |
| --- | --- |
| `kind` | Required. Always `Bundle`. |
| `schema_version` | Required. Must be `2`. |
| `identity` | Required `{ id: "<publisher>/<name>", version: "<semver>", publisher: "<name>" }`. |
| `version_ref` | Optional (`hya`). Used by built-in bundles under `bundles/builtin/` to resolve `0.0.0` to `versions.toml`. |
| `namespace` | Optional tool/schema prefix (defaults to `<name>`; cannot be `mcp`, `harness`, `builtin`, or `plugin`). |
| `overrides` | Optional bundle ID whose primitives this bundle overrides (e.g. `overrides: hya/bare` on `hya/hashline`, or per-tool `overrides: hya/multi-agent` on `hya/agent-channels`'s `wait`). Reserved kernel tools `todo` and `rewrite` can never be overridden. |
| `runtime` | Optional runtime declaration `{ kind: static \| js \| native, mode?, entry?, command?, timeout_ms?, abi_version?, crate_dir?, library?, capabilities? }`. |
| `check` | Optional `{ command: [...], timeout_secs?: 1..600 }` run in an isolated copy during `verify`, `install`, and Bundle CI. |
| `resources` | Optional `{ tools: [...], skills: [...], commands: [...], mcp: [...], hooks: [...] }` (`{ id, path, aliases?, overrides? }`). See the **[Descriptor Schemas](hyabundle-spec.md#4-standardized-descriptor-file-schemas)**. |
| `assets` | Optional list of runtime data files (`[{ id, path }]`, such as JSON Schemas, TUI sources, or config templates). |
| `agents` | Optional list of standalone Markdown agent references (`{ path: "agents/<id>.md" }`) or inline Agent definitions (`id`, `role: main \| subagent`, `description?`, `prompt?`, `model_policy?`, `resource_view?`, `advertise?`, `handoff?: model \| deterministic`, `private?`, `can_spawn?`, `hook_refs?`). |
| `workflow` | Optional `{ id, path: "workflows/*.hya.md" }` (requires `agents` to match the exact reachable closure). |
| `channels` | Optional vertical channel policy templates (`kind: unit \| parent_dm`, `participants`, `capabilities`, `scope`, `retention: team_session`). |
| `schemas` | Optional custom URI schemes (`[{ scheme, tool, writable? }]`) routed through `read`/`write` (e.g. `db://...`; native bundles may also register `UriSchemaHandler` via ABI v2). |
| `apis` | Optional custom HTTP/gRPC endpoints mounted on `hya.v2.BundleApi` and served by a `js` `process` runtime or a native ABI v2 `NativeBundleApiHandler`. |
| `permission_modes` | Optional custom session permission modes (`[{ id, title, description? }]`) backed by `permission.approve`. |
| `tui` | Optional `{ api_version: 2, entry, sdk: "2.0.0", permissions: [...] }` (may be combined with `runtime.kind: native` in hybrid bundles; see **[TUI Extensions](tui-extensions.md)**). |

---

## 3. Advanced Bundle Capabilities

### API endpoints (`apis:` / `BundleApi`)

A bundle with `runtime: { kind: js, mode: process, ... }` (or a native ABI v2 bundle registering `NativeBundleApiHandler`) can mount custom JSON endpoints on the Hya v2 server (`/v2/sessions/{session}/bundles/{bundle}/{path}` or `/v2/bundles/{bundle}/api/{path}`):

```yaml
kind: Bundle
schema_version: 2
identity: { id: acme/notes, version: 1.0.0, publisher: acme }
runtime:
  kind: js
  mode: process
  command: [bun, run, '${BUNDLE_ROOT}/notes.ts']
assets:
  - { id: notes, path: notes.ts }
  - { id: note-schema, path: schemas/note.json }   # packaged JSON Schema
apis:
  # GET /v2/sessions/{session}/bundles/acme%2Fnotes/usage
  - id: usage
    method: GET
    scope: session
    path: /usage
    description: Token usage of the session tree        # optional
  # PUT /v2/bundles/acme%2Fnotes/api/notes/{key}
  - id: put-note
    method: PUT
    scope: global
    path: /notes/{key}
    request_schema: schemas/note.json                   # optional
    response_schema: schemas/note.json                  # optional
```

### Permission modes (`permission_modes:`)

Add selectable modes to `/permissions` (for example `acme/approver/careful` to auto-allow read-only shell commands while deferring everything else to the user) by pairing `permission_modes:` with a `js` `process` runtime (or native hook) that handles `hook/permission.approve`:

```yaml
kind: Bundle
schema_version: 2
identity: { id: acme/approver, version: 1.0.0, publisher: acme }
runtime:
  kind: js
  mode: process
  command: [bun, run, '${BUNDLE_ROOT}/approver.ts']
assets:
  - { id: approver, path: approver.ts }
resources:
  hooks:
    - { id: permission.approve, path: hooks/permission-approve.json }
permission_modes:
  - id: careful
    title: Careful
    description: Approve read-only shell commands; ask for everything else
```

```ts
// approver.ts — standalone JSON-RPC 2.0 permission approver
import { createInterface } from "node:readline"

const READ_ONLY = /^(git (status|diff|log)|ls|cat) /

function approve(params) {
  const { mode, action, resource } = params ?? {}
  if (mode !== "careful" || action !== "bash") return { outcome: "defer" }
  return READ_ONLY.test(`${resource?.value ?? ""} `)
    ? { outcome: "allow_once" }
    : { outcome: "defer" }
}

function reply(id, result) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`)
}

for await (const line of createInterface({ input: process.stdin })) {
  let message
  try {
    message = JSON.parse(line)
  } catch {
    continue
  }
  if (message.id === undefined || message.id === null) continue
  if (message.method === "initialize") {
    reply(message.id, {
      protocol_version: 1,
      plugin: { id: "approver", version: "1.0.0", kind: "bun" },
      hooks: [{ name: "permission.approve" }],
      tools: [],
    })
  } else if (message.method === "hook/permission.approve") {
    reply(message.id, approve(message.params))
  } else {
    reply(message.id, {})
  }
}
```

### User-owned Bundle configuration (`config.yml`)

Never hardcode API keys inside a bundle! Users configure an installed bundle in `~/.config/hya/bundles/<percent-encoded-id>/config.yml` (or `.hya/bundles/<dir>/config.yml` for `--project` installs). Your bundle process and MCP servers receive that path automatically via `HYA_BUNDLE_CONFIG_FILE` and `HYA_BUNDLE_CONFIG_DIR` (or `${BUNDLE_CONFIG_FILE}` in argv).

---

## 4. Importing Claude Code Plugins (`--claude`)

Have a local Claude Code plugin folder (`plugin.json` or `.claude-plugin/plugin.json`) or marketplace entry? Convert and install it directly as a native Hya bundle:

```sh
hya bundle install --claude ./my-claude-plugin
hya bundle install --claude './marketplace#code-review'
```

Hya maps `agents/*.md`, `skills/*/SKILL.md`, `commands/*.md`, `.mcp.json`, and command hooks (`PreToolUse` → `tool.execute.before`, `PostToolUse` → `tool.execute.after`, `PreCompact` → `compaction.before`, `SessionStart`/`SessionEnd`, `SubagentStart` → `agent.spawn`) into a deterministic `.hyabundle`. Unsupported Claude hook events (such as `Stop` or `UserPromptSubmit`) fail import with a clear message rather than being silently ignored.

---

## 5. Tool Plane, Packaging & Activation Contract

Keep these exact runtime invariants in mind when authoring bundles with `agents`, `resource_view`, or Bun sidecars:

- **Origin-derived tool plane:** An installed bundle agent's tool plane is derived from its origin, not declared. Built-in agents see the full Harness plane; a bundle agent sees Hya's built-in public tools plus its own bundle's `resources.{tools,skills,mcp}` (and automatically receives coordination tools `report`, `wait`, `send`, `list_channel`, plus `task`/`archive` when `can_spawn` is non-empty). The clamp is not a sandbox. If `can_spawn` names a built-in agent, that child runs on its own plane.
- **Archive & directory closure:** On packaging, undeclared directory files are ignored; unreferenced archive files are rejected. A compact single-file + JS layout uses `bundle.hya.md extensions/runtime.js` (see [`bun-disjoint`](../examples/bun-disjoint/) where a module exports two tools while two agents each select a different one).
- **Exact-path JS entrypoints & views:** Each selected Tool or Hook source path must exact-path match a JavaScript Extension resource in the owning bundle; only selected Tool/Hook resources determine a deduplicated deterministic entrypoint list—staged does not mean activated. Initialize checks the declarations of the loaded modules, regardless of order, while tools outside `resource_view` and hooks outside `hook_refs` are neither advertised nor dispatched (and any process Skill list is checked against the complete packaged Skill set, independently of the agent's Skill selection).
- **Self-contained activation JS:** The activation-scoped JS profile admits only self-contained selected Extension entrypoints; it does not load inert support files or discover transitive JS imports. Use external single-file bundling before packaging—activation never executes the authoring tree, and a missing relative helper import fails before ACK.
- **Sidecar hooks & JSON-RPC wire:** On an agent's `hook_refs`, `hook_refs` select Bundle-local Hook resources only (all `harness:hook/*` spellings reject; Harness host hooks stay outside Bundle metadata); supported hook IDs are exactly `event`, `tool.execute.before`, and `tool.execute.after`, and aliases do not rename hooks. Sidecars speak newline-delimited JSON-RPC 2.0 on plugin protocol version 1: `initialize` retains existing `protocol_version` and `host` fields, and the only activation-specific metadata is `{ activation_id, lifecycle }`.
