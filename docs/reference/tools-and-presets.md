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
| `bash` | Ask | Execute a shell command (supports `timeout`, `cwd`, `env`, and `pty: true`). |
| `grep` | Allow | Search file contents with a regex or literal pattern (returns `path:line:text` in `hya-bare` or hashline-anchored matches when `hya/hashline` is enabled). |
| `todo` | Allow | Replace/update the session's ordered checklist (`pending`, `in_progress`, `completed`, `blocked`). Permanently kernel-exclusive; cannot be overridden. |
| `rewrite` | Allow | Fold earlier steps in the current turn behind a clean marker while preserving user messages and todos. Permanently kernel-exclusive; cannot be overridden. |
| `skill` | Allow (`read_only`) | Load the full body of a discovered or bundled `SKILL.md` by name. |
| `code-mode__run` | Ask *(opt-in via `tools.code.mode`)* | Run a traced JavaScript async program calling `tools.*` (`workspace.state` supported); dynamically exposes `agent()` / `agents.run` only when `hya/multi-agent` is loaded. |

---

## 2. Registry Tools (Full mode — 24 bundle tools + core `skill`)

In full mode, Hya loads five native tool bundles (`hya/hashline` over Lockstep Native ABI v2 plus the transitional tool families) alongside the kernel-exclusive `todo` and `rewrite`.

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

### `hya/base-tools` (Directory Search, Shell, Patch & User Questions)

| Tool | Default permission | What it does |
| --- | --- | --- |
| `bash` | Ask (`command`, alias `shell`) | Full-mode shell execution with PTY support and multi-root `ProjectScope`. |
| `apply_patch` | Ask (`tool`, alias `patch`) | Multi-file unified patch tool (advertised automatically to `gpt*` / `*codex*` models; hidden alias `patch` works for all models). |
| `ls` | Allow (`read_only`) | List entries in a directory. |
| `glob` | Allow (`read_only`) | Fast breadth-first filename glob matching (up to 100 matches, 30s traversal cap). |
| `find` | Allow (`read_only`) | Structured directory/file search with 30s traversal cap. |
| `ask_user` | Interactive (`tool`, alias `question`) | Ask the user structured multiple-choice or free-text questions (omitted automatically in in-process headless runs). |

### `hya/extended-tools` (Code Intelligence, Subagents & Workflows)

| Tool | Default permission | What it does |
| --- | --- | --- |
| `lsp` | Allow | Query language servers (go-to-definition, references, hover, symbols, diagnostics). |
| `list_agents` | Allow | List spawnable agent definitions in the active catalog. |
| `search_agent` | Allow | Search available agents by keyword. |
| `task` | Allow | Spawn one or more episodic resident subagents in parallel (`tasks[]`). |
| `archive` | Allow | Stop and archive a child subagent (can still be woken later via follow-up mail). |
| `workflow` | Allow | List, inspect, select, run, or check state of compiled `*.hya.md` Workflows. |
| `plan_exit` | Interactive | Hand off a completed plan from `hya-plan` for user approval (omitted in headless runs). |
| `project_activity` | Allow | Inspect recent activity across sessions in the current Project. |

### `hya/network-tools` (Web Access)

| Tool | Default permission | What it does |
| --- | --- | --- |
| `webfetch` | Ask | Fetch a URL and extract readable content (up to 5 MiB; disable via `tools.webfetch.enabled: false`). |
| `websearch` | Ask | Search the web via Exa (default, no key required) or Parallel (`tools.websearch`). |

### `hya/channel-tools` (Parent ⇄ Child Team Messaging)

| Tool | Default permission | What it does |
| --- | --- | --- |
| `send` | Allow | Send mail to a child's `#DM`, a parent (`^parent`), or a unit leader's `#announce` group channel. |
| `list_channel` | Allow | List the caller's channels and unread message counts. |
| `wait` | Allow | Block until child subagents finish/report **or** new mail arrives in the caller's inbox. |
| `report` | Allow *(subagents only)* | Deliver a subagent's final report to its parent, write a state handoff, and archive the episode. |

---

## 3. Built-in Agents (`hya/core-agents`)

| Agent ID | Kind | Tool restrictions | When to use |
| --- | --- | --- | --- |
| `hya-main` | Main *(default)* | Full toolset | Everyday coding, debugging, refactoring, and delegating subtasks. |
| `hya-plan` | Main | Read-only (blocks `write`, `edit`, `apply_patch`, `bash`) | Exploring code and designing a plan before changing any files. |
| `hya-task` | Subagent *(default)* | Full toolset | Focused implementation or testing subtasks delegated via `task`. |
| `hya-scout` | Subagent | Read-only (blocks edits, `bash`, `task`, `archive`) | Fast codebase search and reading without cluttering the main chat. |
| `hya-reviewer` | Subagent | Blocks file edits (`write`, `edit`, `apply_patch`), allows `bash` | Reviewing patches and running check suites without modifying code. |
| `hya-title` | System *(reserved)* | Internal only | Generates short session titles after your first prompt. |

*(Built-in workflow bundles also define private stage agents—`hya-goal-guide`, `hya-goal-verifier`, `hya-pir-planner`, `hya-pir-implementer`, and `hya-pir-reviewer`—used internally by Goal/Loop mode and the `plan-impl-review` Workflow.)*

---

## 4. Built-in Bundles (`bundles/builtin/`)

Every Hya installation ships with 13 trusted first-party bundles sourced from `bundles/builtin/` and packaged under `<prefix>/bundles/hya-<name>.hyabundle` (skipped in `--bare`, which runs only `hya-bare` core tools including `skill` and optional `code-mode`). See the **[`hyabundle` Specification](../extend/hyabundle-spec.md)** and **[ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)** for full paths, runtimes, and loading precedence.

| Bundle ID | Source path | Runtime | Role |
| --- | --- | --- | --- |
| `hya/hashline` | `bundles/builtin/hashline` | Native ABI v2 (`cdylib`) | Hashline-addressed `read`, `write`, `edit`, and `grep` + `HashlineRuntime` (`overrides: hya/bare`, 4 tools). |
| `hya/base-tools` | `bundles/builtin/base-tools` | Native (`cdylib`) | Directory search, shell, patch, and `ask_user` tools (6 tools). |
| `hya/extended-tools` | `bundles/builtin/extended-tools` | Native (`cdylib`) | Code intelligence (`lsp`), subagents, workflows, and project activity (9 tools; `skill` promoted to `hya-bare` core). |
| `hya/network-tools` | `bundles/builtin/network-tools` | Native (`cdylib`) | Web fetch and web search (`webfetch`, `websearch`). |
| `hya/channel-tools` | `bundles/builtin/channel-tools` | Native (`cdylib`) | Team mailbox tools (`send`, `list_channel`, `wait`, `report`). |
| `hya/compaction` | `bundles/builtin/compaction` | Native (`cdylib`) | Context `Rewriter` running `shake` and `remote` compaction before Kernel `Rewrite`. |
| `hya/core-agents` | `bundles/builtin/core-agents` | Static | Built-in agent definitions (`hya-main`, `hya-plan`, `hya-task`, `hya-scout`, `hya-reviewer`, `hya-title`). |
| `hya/core-skills` | `bundles/builtin/core-skills` | Static | Built-in fallback skills (`agent-bundle-authoring`, `secure-self-update`). |
| `hya/core-commands` | `bundles/builtin/core-commands` | Static | Built-in slash command templates (`/init`, `/review`). |
| `hya/agent-channels` | `bundles/builtin/agent-channels` | Static | Default vertical parent/child and unit broadcast channel policy. |
| `hya/goal-loop` | `bundles/builtin/goal-loop` | Static | Goal evaluator and Loop verifier/planner prompts and private helper agents. |
| `hya/plan-impl-review` | `bundles/builtin/plan-impl-review` | Static | Built-in `plan-impl-review` Workflow DAG and stage agents. |
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
