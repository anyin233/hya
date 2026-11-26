# Creating Bundles (`.hyabundle`)

A **Bundle** (`kind: Bundle`) is Hya's all-in-one package format. A single `.hyabundle` file (or project folder under `.hya/bundles/`) can ship:

- **Skills, Tools, MCP servers, and Hooks** for existing agents (*agentless Plugin role*),
- **One or more custom Agents** with scoped permissions and channel policies,
- **A compiled Workflow** with its stage agents,
- **Custom HTTP endpoints** (`apis:`), **custom permission modes** (`permission_modes:`), **URI schemes** (`schemas:`), and **TUI panels** (`tui:`).

Static bundles run zero extra processes. Executable bundles run out-of-process Bun, Rust, or Claude sidecars cleanly isolated from Hya's core engine.

---

## 1. Your first Bundle in 60 seconds

### Single-file Agent Bundle (`bundle.hya.md`)

For a simple custom agent that uses Hya's built-in tools, you only need **one file** named `bundle.hya.md` (see [`docs/examples/bundle.hya.md`](../examples/bundle.hya.md)):

```markdown
---
kind: Bundle
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
cargo run -p xtask -- package-bundle ./sql-helper sql-helper.hyabundle

# Dry-run verification and install (use --project to install into ./.hya/bundles/)
hya bundle verify sql-helper.hyabundle
hya bundle install -y sql-helper.hyabundle

# Use it right away inside Hya with `/agent sql-helper`!
hya bundle list
```

### Multi-file Directory Bundle (`bundle.yaml`)

When packaging multiple agents, a Workflow, skills, or JS/TS extensions, use `bundle.yaml` instead of `bundle.hya.md` (a folder must contain **either** `bundle.yaml` **or** `bundle.hya.md`, never both):

```yaml
kind: Bundle
identity:
  id: acme/review-pack
  version: 1.0.0
  publisher: acme
check:
  command: ["bun", "test"]   # Optional self-test run automatically on verify/install!
  timeout_secs: 120
resources:
  skills:
    - id: review-checklist
      path: skills/review-checklist.md
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
- [`docs/examples/bun-transient/`](../examples/bun-transient/) & [`docs/examples/bun-resident/`](../examples/bun-resident/) — Agent + Bun JS tool (`bundle.hya.md` + `extensions/runtime.js`)
- [`docs/examples/bun-disjoint/`](../examples/bun-disjoint/) — Multi-agent bundle selecting disjoint JS tools and hooks

---

## 2. Manifest Reference (`bundle.yaml`)

Unknown YAML fields are rejected (`deny_unknown_fields`), so typos are caught at package time.

| Top-level key | Purpose |
| --- | --- |
| `kind` | Required. Always `Bundle`. |
| `identity` | Required `{ id: "<publisher>/<name>", version: "<semver>", publisher: "<name>" }`. |
| `namespace` | Optional tool/schema prefix (defaults to `<name>`; cannot be `mcp`, `harness`, `builtin`, or `plugin`). |
| `check` | Optional `{ command: [...], timeout_secs?: 1..600 }` run in an isolated copy during `verify` and `install`. |
| `resources` | Optional `{ tools: [...], skills: [...], mcp: [...], hooks: [...] }` (`{ id, path, aliases? }`). |
| `extensions` | Optional `{ js: [...], files: [...], rust: [...], process?: { kind, command, timeout_ms? } }`. |
| `agents` | Optional list of Agent definitions (`id`, `role: main \| subagent`, `description?`, `prompt?`, `model_policy?`, `resource_view?`, `advertise?`, `handoff?: model \| deterministic`, `private?`, `can_spawn?`, `hook_refs?`). *(Note: legacy `spawn_lifecycle` is removed—all subagents are episodic resident actors.)* |
| `workflow` | Optional `{ id, path: "workflows/*.hya.md" }` (requires `agents` to match the exact reachable closure). |
| `channels` | Optional vertical channel policy templates (`kind: unit \| parent_dm`, `participants`, `capabilities`, `scope`, `retention: team_session`). |
| `schemas` | Optional custom URI schemes (`[{ scheme, tool, writable? }]`) routed through `read`/`write` (e.g. `db://...`). |
| `apis` | Optional custom HTTP/gRPC endpoints served by `extensions.process` (see below). |
| `permission_modes` | Optional custom session permission modes (`[{ id, title, description? }]`) backed by `permission.approve`. |
| `tui` | Optional `{ api_version: 1, entry, sdk: "1.0.0", permissions: [...] }` (see **[TUI Extensions](tui-extensions.md)**). |

---

## 3. Advanced Bundle Capabilities

### API endpoints (`apis:`)

A bundle with an explicit `extensions.process` can mount custom JSON endpoints on the Hya server (`/v1/sessions/{session}/bundles/{bundle}...` or `/v1/bundles/{bundle}/api...`):

```yaml
kind: Bundle
identity: { id: acme/notes, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: bun, command: [bun, run, '${BUNDLE_ROOT}/notes.ts'] }
  files:
    - { id: notes, path: notes.ts }
    - { id: note-schema, path: schemas/note.json }   # packaged JSON Schema
apis:
  # GET /v1/sessions/{session}/bundles/acme%2Fnotes/usage
  - id: usage
    method: GET
    scope: session
    path: /usage
    description: Token usage of the session tree        # optional
  # PUT /v1/bundles/acme%2Fnotes/api/notes/{key}
  - id: put-note
    method: PUT
    scope: global
    path: /notes/{key}
    request_schema: schemas/note.json                   # optional
    response_schema: schemas/note.json                  # optional
```

### Permission modes (`permission_modes:`)

Add selectable modes to `/permissions` (for example `acme/approver/careful` to auto-allow read-only shell commands while deferring everything else to the user) by pairing `permission_modes:` with an `extensions.process` that handles `hook/permission.approve`:

```yaml
kind: Bundle
identity: { id: acme/approver, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: bun, command: [bun, run, '${BUNDLE_ROOT}/approver.ts'] }
  files:
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
