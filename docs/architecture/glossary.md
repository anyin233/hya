# Domain Glossary

Shared terms used across Hya's codebase, APIs, and documentation (aligned with root [`CONTEXT.md`](../../CONTEXT.md)).

---

## Kernel & Context

- **Context $(m, g, w)$:** What a round operates on—the visible transcript ($m$), the ordered session `todo` checklist ($g$), and recorded workspace file observations ($w$).
- **Model append:** Adding the model's own sampled output (text, reasoning, tool calls) to Context—treated as a claim or plan, never outside proof.
- **Tool append:** Adding a deterministic outside observation (a tool result/error paired with a call, or user input).
- **Rewrite:** The kernel's destructive context reset (triggered past 80% window occupancy or via the `rewrite` tool) that drops earlier steps behind a marker repeating all removed user prompts and the current `todo` list.
- **Compaction (`hya/compaction`):** Full-mode native extension steps (`shake` tool-output eviction to `artifact://` and `remote` provider `/responses/compact`) that run *before* the kernel's `Rewrite`.
- **Bare mode (`hya --bare`, `hya serve --bare`, `hya exec --bare`):** Running the unified `SessionEngine` with `BuiltinBundleProfile::Bare`—zero built-in bundles loaded, the 8 standard bare tools (`read`, `write`, `edit`, `bash`, `grep`, `todo`, `rewrite`, `skill`), built-in `code-mode` (without `agent()`), the minimal `hya-bare` default agent, and standard `AGENTS.md`/skills/MCP/plugin adapters. Combine with `--pure` (`hya exec --bare --pure`) to skip external context and plugins.

---

## Sessions, Agents & Bundles

- **Session (`hysec_...`):** One persisted, event-sourced conversation in `event_log`. Both main chats and spawned subagents are full Sessions.
- **Turn:** One cycle from prompt/command/shell admission through 0+ provider/tool rounds to `MessageFinished`.
- **Agent:** A role definition (ID, prompt, model policy, `resource_view`, `can_spawn`, `hook_refs`). Its **Origin** is either **built-in** (`hya/core-agents`, full Harness tool plane) or **bundle** (clamped `InternalPublic` plane + its own bundle's resources).
- **Bundle (`.hyabundle`, format v4):** Hya's deterministic package unit (`kind: Bundle`), carrying any mix of agents, a compiled Workflow, skills, tools, MCP servers, hooks, sidecar processes, custom HTTP APIs, permission modes, and TUI extensions.

---

## Subagents, Teams & Workflows

- **Team:** The tree of Sessions rooted at one **Main agent** (`handle = "main"`, max depth `2`).
- **Subagent (Member):** A child Session spawned via `task` (or a Workflow Stage), named deterministically as `main/<type>-<operator>`. Every subagent is an **episodic resident actor**.
- **Episode & Handoff:** One bounded activation of a subagent from spawn/revival until `report` (or `archive`). Ending an episode writes a state **Handoff** summary, releases its concurrency budget, and archives the subagent; a downward `#DM` from its parent revives it into episode `n + 1` with that Handoff.
- **Channel (`#DM-{8}` & `#announce-{8}`):** Strictly vertical parent ⇄ direct-child mail pipes recorded on the team-root `event_log`. Siblings never see or message each other.
- **Workflow (`*.hya.md`):** A compiled, acyclic graph of **Stages** (`hya-workflow`) executed durably by `hya-engine` on resident subagent sessions. *(Distinct from a dynamic JS **Workflow program** in `hya-extra/orchestrator`.)*

---

## Workspaces, Daemon & Relay

- **Backend daemon:** The detached `hya serve` process owning a SQLite database via `<db>.runtime-owner.lock` and `<db>.server.json`.
- **Project (`prj_...`) & Project roots:** A client-chosen workspace consisting of one or more ordered absolute folders on the backend host. File tools access paths inside any root freely and prompt `ExternalDirectory` outside (`ProjectScope`).
- **Catalog scope:** The `Global`, `Directory(path)`, or `Project { id, roots }` key used to lazily compose and cache active bundles, plugins, skills, commands, and `AGENTS.md`.
- **Temporary vs. Ephemeral session:** A **Temporary session** (`--temp`) works in `~/.cache/hya/scratch/<id>` outside any Project (never auto-deleted). An **Ephemeral session** (`ephemeral: true`) is an unused pre-opened root session that the daemon reaps ~5s after the last client disconnects.
- **Relay, Room, Relay link & Bridge:** Remote access pieces (`hya-relay`): `hya proxy` hosts blind **Rooms**; `hya serve --relay` registers the Room and prints the secret **`hya://` link**; `hya bridge` / `hya --connect` opens a local loopback listener tunneling `hya.v1` inside end-to-end **`Noise_NKpsk0`** encryption.
