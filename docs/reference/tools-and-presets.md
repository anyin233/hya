# Built-in Tools, Agents & Bundles

This page lists every tool, agent role, first-party bundle, and optional extra bundle shipped with Hya.

---

## 1. Kernel Tools (always available, including `hya exec --bare`)

These six foundational tools live directly inside Hya's core kernel (`hya-core`):

| Tool | Default permission | What it does |
| --- | --- | --- |
| `read` | Allow *(inside Project)* | Read text files (with `LINE#HASH` anchors), images (`png`/`jpeg`/`gif`/`webp`), PDFs, directories, `artifact://` spilled outputs, and `channel://` mail logs. |
| `write` | Ask | Create or overwrite a file on disk (runs post-edit formatter & LSP diagnostics in full mode). |
| `edit` | Ask | Edit an existing file via exact text replacement or `LINE#HASH` line anchors. |
| `bash` | Ask | Execute a shell command (supports `timeout`, `cwd`, `env`, and `pty: true`). |
| `todo` | Allow | Replace/update the session's ordered checklist (`pending`, `in_progress`, `completed`, `blocked`). |
| `rewrite` | Allow | Fold earlier steps in the current turn behind a clean marker while preserving user messages and todos. |

---

## 2. Registry Tools (Full mode — 25 canonical tools)

In full mode, Hya loads four native tool-family bundles alongside the kernel's `todo` and `rewrite`. (`read`, `write`, `edit`, and `bash` gain full-mode permission checks, `ProjectScope` folder containment, and LSP/formatter hooks.)

### `hya/base-tools` (File, Search, Shell & User Questions)

| Tool | Default permission | What it does |
| --- | --- | --- |
| `read`, `write`, `edit`, `bash` | See above | Full-mode file and shell tools with multi-root `ProjectScope` enforcement. |
| `apply_patch` | Ask | Multi-file unified patch tool (advertised automatically to `gpt*` / `*codex*` models; hidden alias `patch` works for all models). |
| `ls` | Allow | List entries in a directory. |
| `glob` | Allow | Fast breadth-first filename glob matching (up to 100 matches, 30s traversal cap). |
| `find` | Allow | Structured directory/file search with 30s traversal cap. |
| `grep` | Allow | Fast regular-expression content search across files. |
| `ask_user` | Interactive | Ask the user structured multiple-choice or free-text questions (omitted automatically in in-process headless runs). |

### `hya/extended-tools` (Code Intelligence, Skills, Subagents & Workflows)

| Tool | Default permission | What it does |
| --- | --- | --- |
| `lsp` | Allow | Query language servers (go-to-definition, references, hover, symbols, diagnostics). |
| `skill` | Allow | Load the full body of a discovered or bundled `SKILL.md` by name. |
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

## 4. The 13 Built-in Bundles (`bundles/builtin/`)

Every Hya installation ships with 13 trusted first-party bundles sourced from `bundles/builtin/` and packaged under `<prefix>/bundles/hya-<name>.hyabundle` (skipped in `hya exec --bare`, which runs only the 6 kernel tools). See the **[`hyabundle` Specification](../extend/hyabundle-spec.md)** and **[ADR-0038](../adr/0038-hyabundle-standard-and-independent-ci.md)** for full paths, runtimes, and loading precedence.

| Bundle ID | Source path | Runtime | Role |
| --- | --- | --- | --- |
| `hya/base-tools` | `bundles/builtin/base-tools` | Native (`cdylib`) | Core file, search, shell, patch, and `ask_user` tools (10 tools). |
| `hya/extended-tools` | `bundles/builtin/extended-tools` | Native (`cdylib`) | Code intelligence (`lsp`), skills, subagents, workflows, and project activity (9 tools). |
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
| `hya/code-mode` | `bundles/builtin/code-mode` | JS (`process: bun`) | Optional `code-mode__run` JavaScript batch tool (`tools.code.mode`, off by default). |

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
