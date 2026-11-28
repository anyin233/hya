# Built-in Tools, Agents & Bundles

This page lists every tool, agent role, first-party bundle, and optional extra bundle shipped with Hya.

---

## 1. Core `hya-bare` Tools & Code Mode (always available, including `--bare`)

These core tools live directly inside Hya's core kernel (`hya-core`), alongside the optional core Code Mode runtime (`packages/hya-code-mode` + `crates/hya-app/src/code_mode.rs`):

| Tool | Default permission | What it does |
| --- | --- | --- |
| `read` | Allow *(inside Project)* | Read text files (with `LINE#HASH` anchors when `hya/hashline` is enabled; standard verbatim/offset text when disabled or under `--bare`), images (`png`/`jpeg`/`gif`/`webp`), directories, internal handles (`artifact://`, `skill://`, `local://`), and any registered custom URI scheme via `HandleRouter`. |
| `write` | Ask | Create or overwrite a file on disk, `local://<name>`, or any registered writable custom URI scheme (post-edit formatting/LSP run via hooks rather than hardcoded calls). |
| `edit` | Ask | Edit an existing file via standard exact text replacement (`hya-bare`) or content-hashed `LINE#HASH` anchors (`hya/hashline`). |
| `bash` | Ask (`command`, alias `shell`) | Execute a shell command (`hya-core` kernel primitive; supports `timeout`, `cwd`, run-budget yielding, and coding output caps). |
| `grep` | Allow | Search file contents with a regex or literal pattern (returns `path:line:text` in `hya-bare` or hashline-anchored matches when `hya/hashline` is enabled). |
| `todo` | Allow | Replace/update the session's ordered checklist (`pending`, `in_progress`, `completed`, `blocked`). Permanently kernel-exclusive; cannot be overridden. |
| `rewrite` | Allow | Fold earlier steps in the current turn behind a clean marker while preserving user messages and todos. Permanently kernel-exclusive; cannot be overridden. |
| `skill` | Allow (`read_only`) | Load the full body of a discovered or bundled `SKILL.md` by name. |
| `code-mode__run` | Ask *(opt-in via `tools.code.mode`)* | Run a traced JavaScript async program calling `tools.*` (`workspace.state` supported); dynamically exposes `agent()` / `agents.run` only when `hya/multi-agent` is loaded. |

---

## 2. Registry Tools (Full mode — 23 bundle tools + core `bash` & `skill`)

In full mode, Hya loads all of its native tool bundles (`hya/hashline`, `hya/coding-extras`, `hya/multi-agent`, `hya/network`, `hya/agent-channels`, `hya/plan`, and `hya/workflow` — all over Lockstep Native ABI v2) alongside core `bash`, `skill`, and the kernel-exclusive `todo` and `rewrite`.

### `hya/hashline` (Content-Hashed File Tools — `overrides: hya/bare`)

`hya/hashline` is a Lockstep Native ABI v2 bundle (`bundles/builtin/hashline/`, `schema_version: 2`, exported via `export_native_bundle_v2!` / `hya_native_register_v2`) that overrides `hya-bare`'s standard `read`, `write`, `edit`, and `grep` with `HashlineRuntime`-backed `LINE#HASH` line-addressed primitives. Custom URI schemes delegate generically through `HandleRouter` (`DynamicUriHandler` / `UriSchemaHandler`), and post-edit formatting/LSP run through host/bundle hooks rather than hardcoded library calls.

**Opting out (falling back to standard `hya-bare` file tools):**

```yaml
# ~/.config/hya/config.yaml
schema_version: 2
bundles:
  builtin:
    profile: full
    disable:
      - hya/hashline
```

When `hya/hashline` is disabled (or under `--bare` / `profile: bare`), `hya/hashline` is not loaded and `hya-bare`'s standard `read`, `write`, `edit`, and `grep` take over cleanly.

| Tool | Default permission | Descriptor / Overrides | What it does |
| --- | --- | --- | --- |
| `read` | Allow (`read_only`, `protected: true`) | `tools/read.json` (`overrides: hya/bare`) | Read UTF-8 files with `LINE#HASH` anchors (or `raw: true`), directories, images/PDFs, internal handles, and custom `HandleRouter` URIs. |
| `write` | Ask (`tool`) | `tools/write.json` (`overrides: hya/bare`) | Atomic whole-file write with optional copied hashline prefix stripping, `local://` scratch handles, and writable custom `HandleRouter` URIs. |
| `edit` | Ask (`tool`) | `tools/edit.json` (`overrides: hya/bare`) | Strict hashline edits (`replace`, `append`, `prepend` by `LINE#HASH` anchor, or `replace_text`) with stale-anchor recovery. |
| `grep` | Allow (`read_only`) | `tools/grep.json` (`overrides: hya/bare`) | Regex or literal content search returning hashline-anchored matches and recording recovery snapshots. |

### `hya/coding-extras` (Workspace Search, Patching, User Questions, LSP & Formatters)

`hya/coding-extras` is a Lockstep Native ABI v2 bundle (`bundles/builtin/coding-extras/`, `schema_version: 2`, `capabilities: [tools, hooks, services]`, exported via `export_native_bundle_v2!` / `hya_native_register_v2`) that packages workspace ergonomics, code intelligence, and automatic post-edit formatting/diagnostics on top of `hya-bare`.

**Configuration & Usage:**

Configure formatters and LSP language servers in `~/.config/hya/bundles/hya%2Fcoding-extras/config.yml` (see `bundles/builtin/coding-extras/config.example.yml`):

```yaml
# ~/.config/hya/bundles/hya%2Fcoding-extras/config.yml
formatter:
  rustfmt:
    command: ["rustfmt", "$FILE"]
    extensions: [".rs"]
lsp:
  rust-analyzer:
    command: ["rust-analyzer"]
    extensions: [".rs"]
```

Set `formatter: false` or `lsp: false` to disable either subsystem, or disable the entire bundle in `~/.config/hya/config.yaml` with `bundles.builtin.disable: [hya/coding-extras]`.

**Tools (7):**

| Tool | Default permission | Descriptor / Aliases | What it does |
| --- | --- | --- | --- |
| `ls` | Allow (`read_only`) | `tools/ls.json` | List directory entries as a compact tree (skipping ignored directories). |
| `glob` | Allow (`read_only`) | `tools/glob.json` | Fast breadth-first filename glob matching (up to 100 matches, 30s traversal cap). |
| `find` | Allow (`read_only`) | `tools/find.json` | Structured directory/file search with 30s traversal cap. |
| `apply_patch` | Ask (`tool`) | `tools/apply_patch.json` (hidden alias `patch`) | Multi-file unified patch tool (advertised automatically to `gpt*` / `*codex*` models; hidden alias `patch` works for all models). |
| `ask_user` | Interactive (`interactive`) | `tools/ask_user.json` (hidden alias `question`) | Ask the user structured multiple-choice or free-text questions. |
| `lsp` | Allow (`read_only`) | `tools/lsp.json` | Query language servers (`goToDefinition`, `findReferences`, `hover`, `documentSymbol`, `workspaceSymbol`, `goToImplementation`, `prepareCallHierarchy`, `incomingCalls`, `outgoingCalls`). |
| `project_activity` | Allow (`read_only`) | `tools/project_activity.json` | Inspect recent activity across sessions in the current Project. |

**Service & Native Hook Interfaces:**

- **Service (`lsp-formatter`):** Implements `hya_runtime_abi::NativeService`. Reads `formatter` (`bool | map<name, { command, extensions, environment, disabled }>`) and `lsp` (`bool | map<name, { command, extensions, disabled, env, initialization }>`) from `hya/coding-extras` bundle config on `start()`, reports readiness via `health()`, and shuts down spawned LSP server subprocesses on `stop()`.
- **Post-Edit Hook (`coding-extras.post-edit` on `HookPoint::ToolExecuteAfter`):** Intercepts successful non-noop `write` and `edit` results for both standard `hya-bare` file tools (`metadata.path`) and `hya/hashline` file tools (`metadata.path` / `metadata.filediff.file`), runs the configured formatter while preserving any UTF-8 BOM, notifies LSP (`touch_file`), and attaches LSP error diagnostics to `metadata.diagnostics` and the tool `output` text.
- **Interactive Visibility Hook (`coding-extras.ask-user-visibility` on `HookPoint::ToolVisibility`):** Inspects `ToolVisibilityInput.interaction_available` and returns `ToolVisibilityOutcome::Hide { tools: ["ask_user"] }` whenever no interactive human receiver is connected.

### `hya/multi-agent` (Subagents, Resident Supervision & Team Control — Native ABI v2)

`hya/multi-agent` (`bundles/builtin/multi-agent/`, Lockstep Native ABI v2, `schema_version: 2`, `capabilities: [tools, hooks, services, apis]`) packages Hya's multi-agent orchestration subsystem—subagent tools, built-in worker agents (`hya-task`, `hya-scout`, `hya-reviewer`), fenced actor claim persistence, handle naming, subagent concurrency governor, terminal handoffs, projection folding, and user control APIs—running solely over `Arc<dyn HostRuntime>`.

**Usage & Configuration:**
- Enabled by default in `profile: full`; disable via `bundles.builtin.disable: [hya/multi-agent]` in `~/.config/hya/config.yaml` (or `--bare`) to remove subagent tools, worker agents, and Code Mode's `agent()` / `agents.run` bridge cleanly.
- Configure subagent limits in `~/.config/hya/bundles/hya%2Fmulti-agent/config.yml`:
  ```yaml
  subagents:
    max_concurrency: 100
    per_run_budget: 1024
    per_team_turn_budget: 1024
    per_team_message_budget: 1024
  ```
- Example tool call:
  ```json
  {
    "tool": "task",
    "input": {
      "tasks": [
        { "agent": "hya-scout", "handle_prefix": "scout", "description": "Find auth routes", "prompt": "Locate all auth handlers and return path:line references." }
      ]
    }
  }
  ```

**Tools (5):**

| Tool | Default permission | Descriptor | What it does |
| --- | --- | --- | --- |
| `list_agents` | Allow (`read_only`) | `tools/list_agents.json` | List spawnable agent definitions in the active catalog (`{ "agents": [...] }`). |
| `search_agent` | Allow (`read_only`) | `tools/search_agent.json` | Search available agents by keyword (`{ "query": string }`). |
| `task` | Allow (`task`) | `tools/task.json` | Spawn one or more episodic resident subagents in parallel (`tasks[]`, `wait?: boolean`). |
| `archive` | Allow (`task`) | `tools/archive.json` | Stop and archive a child subagent (`{ "target": string, "reason"?: string }`); wakes again on follow-up mail. |
| `wait` | Allow (`read_only`) | `tools/wait.json` | Block until direct child subagents finish/report (`{ "targets"?: string[], "mode"?: "any" \| "all", "timeout_secs"?: integer }`; overridden by `hya/agent-channels`'s mail-aware `wait` when both bundles are enabled). |

**Interfaces exposed by `hya/multi-agent`:**
- **Built-in Subagents (3):** `hya-task` (full toolset, `can_spawn: [hya-task, hya-scout, hya-reviewer, hya-plan]`), `hya-scout` (read-only), and `hya-reviewer` (read-only + `bash`, `can_spawn: [hya-scout]`).
- **Service (`hya/multi-agent:service`):** Implements `NativeService`; persists fenced actor lease claims (`bundle_claim`) and team checkpoints (`bundle_state`) under namespace `"hya/multi-agent"` via `HostRuntime::bundle_storage()`.
- **Lifecycle Hook (`hya/multi-agent:lifecycle`):** Subscribes to `HookPoint::SessionEnd` to release session actor claim leases on teardown.
- **Projection Reducer (`ProjectionReducer`):** Folds `hya/multi-agent` `Event::Extension` records (`member_spawned`, `member_status_changed`, `member_finished`, `agent_registered`, `agent_activity_changed`, `resident_work_started`, `resident_work_finished`, `agent_heartbeat`, `subagent_reported`, `handoff_committed`, `agent_archived`, `agent_restarted`) into `projection.extensions["hya/multi-agent"]`.
- **`NativeBundleApiHandler` (`apis`):**
  - `GET /v2/sessions/{id}/bundles/hya%2Fmulti-agent/members` & `GET /v2/bundles/hya%2Fmulti-agent/api/members?session=<id>` → `{ "root", "roster": [...], "archived": [...], "members": [...] }`
  - `POST /v2/sessions/{id}/bundles/hya%2Fmulti-agent/archive` & `POST /v2/bundles/hya%2Fmulti-agent/api/archive` (`{ "session": "<id>" }`) → `{ "handle", "cancelledTurn", "descendants" }`
  - `POST /v2/sessions/{id}/bundles/hya%2Fmulti-agent/steer` (`{ "text": string }`) & `POST /v2/bundles/hya%2Fmulti-agent/api/steer` (`{ "session": "<id>", "text": string }`) → `{ "from" }`

### `hya/plan` (Interactive Plan Mode — Native ABI v2)

`hya/plan` packages Hya's read-only planning workflow as a self-contained native ABI v2 bundle (`bundles/builtin/plan`, `id: hya/plan`), combining the `hya-plan` agent, the `plan_exit` approval tool, and its `tool.visibility` native hook so plan mode can be enabled or disabled independently without hardcoding plan logic in `hya-engine`.

**Usage:**
- In interactive TUI/WebUI sessions, switch to the planner with `/agent hya-plan` (or configure `default_agent: hya-plan`). When planning finishes, `hya-plan` calls `plan_exit` to request user approval before implementation begins.
- In headless runs (no interactive interaction receiver), the bundle's `tool.visibility` hook automatically hides `plan_exit` from advertised tool schemas and nested tool listings.
- Disable the bundle in `config.yaml` via `bundles.builtin.disable: [hya/plan]` (or run `hya --bare`).

**Interfaces exposed by `hya/plan`:**
- **Tool `plan_exit`** (hidden alias `plan`, permission `Tool`):
  - Input schema: `{ "plan": string (required), "title"?: string, "summary"?: string }`.
  - Output: `{ "approved": boolean, "action": "approved" | "declined", "title": string, "plan": string, "summary"?: string, "feedback"?: string }`.
- **Agent `hya-plan`** (`role: main`): read-only (`resource_view.deny: [write, edit, apply_patch, bash]`), may spawn `hya-scout`, `hya-task`, and `hya-reviewer`.
- **Native Hook `tool.visibility`** (`plan_exit_visibility`): receives `ToolVisibilityInput { session, agent, candidate_tools, interactive }` and returns `ToolVisibilityOutcome::Hide { tools: ["plan_exit"] }` when `interactive == false`, or `ToolVisibilityOutcome::Preserve` otherwise.

| Tool | Default permission | What it does |
| --- | --- | --- |
| `plan_exit` | Interactive | Hand off a completed plan from `hya-plan` for user approval (hidden alias `plan`; hidden automatically in headless runs by `hya/plan`'s `tool.visibility` native hook). |

### `hya/network` (Web Access & Air-Gapped Control)

`hya/network` (`bundles/builtin/network`, native ABI v2) owns all internet-access tools (`webfetch`, `websearch`) and their search backend configuration outside `hya-tool` and `hya-app`.

| Tool | Hidden Alias | Default permission | What it does |
| --- | --- | --- | --- |
| `webfetch` | `fetch` | Ask | Fetch an HTTP(S) URL as `markdown`, `text`, or `html` (up to 5 MiB, default 30s timeout capped at 120s). |
| `websearch` | `search` | Ask | Search the web via Exa (default, no key required) or Parallel. |

- **Per-bundle configuration (`~/.config/hya/bundles/hya%2Fnetwork/config.yml`, see `bundles/builtin/network/config.example.yml`):**
  ```yaml
  webfetch:
    enabled: true
    max_response_bytes: 5242880
    default_timeout_secs: 30
    max_timeout_secs: 120
  websearch:
    enabled: true
    provider: exa          # exa | parallel
    # endpoint: https://mcp.exa.ai/mcp
    # key: ${env:PARALLEL_API_KEY}
  ```
- **Air-gapped mode:** Disable the bundle in `~/.config/hya/config.yaml` to remove `webfetch`, `websearch`, `fetch`, and `search` cleanly from the runtime without loading `libhya_network`:
  ```yaml
  schema_version: 2
  bundles:
    builtin:
      profile: full
      disable: [hya/network]
  ```
- **Interface definitions:**
  - `webfetch` input schema: `{ "url": string, "format"?: "text" | "markdown" | "html", "timeout"?: number }`
  - `websearch` input schema: `{ "query": string, "numResults"?: number, "livecrawl"?: "fallback" | "preferred", "type"?: "auto" | "fast" | "deep", "contextMaxCharacters"?: number }`
  - Native ABI v2 exports (`hya_native_register_v2`): tools `webfetch` & `websearch`, service `hya/network:service` (`NativeService` reading `HostCatalogReader::bundle_config("hya/network")`), and visibility hook `hya/network:visibility` (`HookPoint::ToolVisibility`).

### `hya/agent-channels` (Parent ⇄ Child Team Messaging, Mailbox & `channel://` URI Schema)

`hya/agent-channels` (`bundles/builtin/agent-channels/`, Lockstep Native ABI v2) unifies team mailbox delivery, declarative vertical channel policies (`unit-default` and `parent-dm-default`), the `channel://` `UriSchemaHandler`, in-turn unread-mail steering (`HookPoint::ToolExecuteAfter`), and read-only session `BundleApi` inspection endpoints into a single native+static bundle running solely over `Arc<dyn HostRuntime>`.

| Tool | Default permission | Descriptor / Overrides | What it does |
| --- | --- | --- | --- |
| `send` | Allow (`tool`) | `tools/send.json` | Send mail to a child's `#DM`, a parent (`^parent`), or a unit leader's `#announce` group channel (reviving archived direct children). |
| `list_channel` | Allow (`read_only`) | `tools/list_channel.json` | List the caller's visible channels, unread message counts, and direct-child heartbeat status. |
| `wait` | Allow (`read_only`) | `tools/wait.json` (`overrides: hya/multi-agent`) | Block until child subagents finish/report **or** new mail arrives in the caller's inbox. |
| `report` | Allow *(subagents only)* | `tools/report.json` | Deliver a subagent's final report to its parent, write a state handoff, and archive the episode. |

- **Usage & configuration:**
  - Loaded by default in `profile: full`; disable with `bundles.builtin.disable: [hya/agent-channels]` or `--bare` to omit team channels, `channel://` resolution, and unread-mail steering.
  - Agents read channel history through the standard `read` tool (`read channel://<id>` or `read channel://<id>?last=N`, or `read channel://<member-handle>` for a DM alias). Because `ChannelUriSchemaHandler` registers directly on `HandleRouter`, `channel://` reads work identically whether `hya/hashline` `read` or standard `hya-bare` `read` is active.
  - Example:
    ```json
    { "tool": "send", "input": { "channel": "main/scout-1", "body": "check crates/hya-engine" } }
    { "tool": "read", "input": { "path": "channel://DM-aB12Cd34?last=5" } }
    ```
- **Interface definitions:**
  - **Manifest capabilities:** `runtime: { kind: native, abi_version: 2, crate_dir: native, capabilities: [tools, hooks, services, schemas, apis] }` plus `channels: [unit-default, parent-dm-default]`.
  - **URI schema (`UriSchemaHandler`):** `channel://<id>[?last=N]` (read-only; clamps `last` to `1..=50`, default `1`; returns `text/plain` messages plus structured metadata `{ type: "channel", channel, count, unreadRemaining, warning }` and commits `mail_consumed`).
  - **Native hook (`NativeHook`):** `unread-mail-steer` on `HookPoint::ToolExecuteAfter` splices `[NEW MAIL · answer or acknowledge via dm]` notices into successful non-`wait`/`report` tool results and stages `mail_consumed` for atomic commit with `ToolResult`.
  - **Projection reducer (`ProjectionReducer`):** Folds `hya/agent-channels` `Event::Extension` records (`channel_created`, `channel_joined`, `mail_sent`, `mail_consumed`) into `projection.extensions["hya/agent-channels"]` (`{ mail, inboxes, channels, cursors }`).
  - **Session `BundleApi` endpoints** (`/v2/sessions/{session}/bundles/hya/agent-channels/...`):
    - `GET /channels` (`channels.list`) → `{ "channels": [{ "id", "kind", "can_speak", "peer", "unread" }], "team": [{ "handle", "status", "last_active_seconds" }] }`
    - `GET /channels/{channel}/messages?last=N` (`channels.messages`, read-only peek without advancing cursor, `last` default `20`, max `100`) → `{ "channel", "count", "unread_remaining", "warning", "messages": [{ "from", "body" }] }`
    - `GET /inbox` (`channels.inbox`, read-only) → `{ "handle", "cursor", "total", "unread": [{ "from", "channel", "body" }] }`
    - `GET /team-status` (`channels.team_status`) → `{ "members": [{ "handle", "status", "last_active_seconds" }] }`

---

## 3. Built-in Agents (`hya/core-agents`, `hya/multi-agent` & `hya/plan`)

| Agent ID | Bundle | Kind | Tool restrictions | When to use |
| --- | --- | --- | --- | --- |
| `hya-main` | `hya/core-agents` | Main *(default)* | Full toolset | Everyday coding, debugging, refactoring, and delegating subtasks. |
| `hya-plan` | `hya/plan` | Main | Read-only (blocks `write`, `edit`, `apply_patch`, `bash`) | Exploring code and designing a plan before changing any files. |
| `hya-task` | `hya/multi-agent` | Subagent *(default)* | Full toolset | Focused implementation or testing subtasks delegated via `task`. |
| `hya-scout` | `hya/multi-agent` | Subagent | Read-only (blocks edits, `bash`, `task`, `archive`) | Fast codebase search and reading without cluttering the main chat. |
| `hya-reviewer` | `hya/multi-agent` | Subagent | Blocks file edits (`write`, `edit`, `apply_patch`), allows `bash` | Reviewing patches and running check suites without modifying code. |
| `hya-title` | `hya/core-agents` | System *(reserved)* | Internal only | Generates short session titles after your first prompt. |

*(Built-in workflow bundles also define private stage agents—`hya-goal-guide`, `hya-goal-verifier`, `hya-pir-planner`, `hya-pir-implementer`, and `hya-pir-reviewer`—used internally by Goal/Loop mode and the `plan-impl-review` Workflow.)*

---

## 4. Built-in Bundles (`bundles/builtin/`)

Every Hya installation ships with 14 trusted first-party bundles sourced from `bundles/builtin/` and packaged under `<prefix>/bundles/hya-<name>.hyabundle` (skipped in `--bare`, which runs only `hya-bare` core tools including `skill` and optional `code-mode`). See the **[`hyabundle` Specification](../extend/hyabundle-spec.md)** and **[ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)** for full paths, runtimes, and loading precedence.

| Bundle ID | Source path | Runtime | Role |
| --- | --- | --- | --- |
| `hya/hashline` | `bundles/builtin/hashline` | Native ABI v2 (`cdylib`) | Hashline-addressed `read`, `write`, `edit`, and `grep` + `HashlineRuntime` (`overrides: hya/bare`, 4 tools). |
| `hya/coding-extras` | `bundles/builtin/coding-extras` | Native ABI v2 (`cdylib`) | Workspace search (`ls`, `glob`, `find`), `apply_patch`, `ask_user`, `lsp`, `project_activity`, `lsp-formatter` `NativeService`, and post-edit + visibility `NativeHook`s (7 tools). |
| `hya/multi-agent` | `bundles/builtin/multi-agent` | Native ABI v2 (`cdylib`) | Subagent orchestration: 5 tools (`list_agents`, `search_agent`, `task`, `archive`, `wait`), 3 subagents (`hya-task`, `hya-scout`, `hya-reviewer`), fenced actor claims (`bundle_claim`), `ProjectionReducer`, `SessionEnd` hook, and `BundleApi` (`/members`, `/archive`, `/steer`). |
| `hya/network` | `bundles/builtin/network` | Native ABI v2 (`cdylib`) | Web fetch and web search (`webfetch`, `websearch`) + per-bundle `config.yml` and air-gapped disable. |
| `hya/agent-channels` | `bundles/builtin/agent-channels` | Native ABI v2 (`cdylib`) + static | Unified team mailbox & channels (`send`, `list_channel`, `report`, mail-aware `wait` overriding `hya/multi-agent`), `channel://` `UriSchemaHandler`, `unread-mail-steer` `NativeHook`, read-only `BundleApi`, `ProjectionReducer`, and default vertical channel policies (`unit-default`, `parent-dm-default`). |
| `hya/compaction` | `bundles/builtin/compaction` | Native ABI v2 (`cdylib`) | Context `Rewriter` (`shake` + `remote`) and `ProjectionReducer` (`Event::Extension`) before Kernel `Rewrite`. |
| `hya/plan` | `bundles/builtin/plan` | Native ABI v2 (`cdylib`) | Read-only `hya-plan` agent, `plan_exit` tool (alias `plan`), and `tool.visibility` native hook. |
| `hya/goal` | `bundles/builtin/goal` | Native ABI v2 (`cdylib`) | Autonomous goal-mode `GoalDriver` (`hya -p`), `GoalDriverService`, 3 goal skills (`goal-contract`, `guided-goal`, `evaluator-prompt`), and 2 private helper agents (`hya-goal-guide`, `hya-goal-verifier`). |
| `hya/loop` | `bundles/builtin/loop` | Native ABI v2 (`cdylib`) | Verifier/planner `LoopDriver` (`hya loop`), `LoopService`, and `loop-verifier-prompt` / `loop-planner-prompt` skills (`capabilities: [services, drivers]`). |
| `hya/core-agents` | `bundles/builtin/core-agents` | Static | Core built-in agent definitions (`hya-main`, `hya-title`). |
| `hya/core-skills` | `bundles/builtin/core-skills` | Static | Built-in fallback skills (`agent-bundle-authoring`, `secure-self-update`). |
| `hya/core-commands` | `bundles/builtin/core-commands` | Static | Built-in slash command templates (`/init`, `/review`). |
| `hya/workflow` | `bundles/builtin/workflow` | Native ABI v2 (`cdylib`) | Workflow DAG execution, durable replay, `workflow` tool, `ProjectionReducer` (`extensions["hya/workflow"]`), `BundleApi` (`/workflows`, `/state`, `/command`), and the built-in `plan-impl-review` Workflow + stage agents (`capabilities: [tools, services, apis]`). |
| `hya/basic-tui-components` | `bundles/builtin/basic-tui-components` | JS / TUI | Standard TUI panels (Projects view/sidebar, Sessions, Todos, Context). |

---

## 5. Optional Extra & Example Bundles (`bundles/extra/`, `bundles/examples/`)

Source checkouts also include six optional official bundles under `bundles/extra/` (plus runnable examples such as `bundles/examples/argus-example`) that you can package with `hya bundle pack <dir> <out.hyabundle>`, verify with `hya bundle verify --strict --protocol-smoke`, and install when needed:

| Bundle ID | What it adds |
| --- | --- |
| `hya-extra/zvec-grep` | Connects the `zvec-grep` semantic code search MCP server + guidance skill. |
| `hya-extra/scout` | Read-only `scout` subagent powered by semantic `zvec-grep` + `read`/`grep`/`glob`. |
| `hya-extra/jev-model-router` | `chat.params` hook that classifies task difficulty (easy/medium/hard) via TypeSafe Jev and routes models per session chain. |
| `hya-extra/model-fallback` | `model.fallback` hook providing per-model custom fallback chains when a provider fails before streaming. |
| `hya-extra/token-summary` | Adds a `token-summary__token_summary` tool and `GET /v1/sessions/{id}/bundles/hya-extra%2Ftoken-summary/usage` API reporting token spend across a whole team tree. |
| `hya-extra/orchestrator` | Main `orchestrator` agent that coordinates dedicated `orchestrator-scout`, `orchestrator-editor`, and `orchestrator-runner` workers via traced JavaScript workflow programs. |
