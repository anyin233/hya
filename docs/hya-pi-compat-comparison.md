# hya, Pi, and Compat Feature Comparison

Last researched: 2026-06-30.

This page compares hya with upstream stock Pi (`earendil-works/pi`) and current
Compat (`anomalyco/compat` plus `compat.ai`). It intentionally does not
use the Oh My Pi harness/fork as the Pi baseline, and it does not use the
archived `compat-ai/compat` repository as the Compat baseline.

## Executive Summary

| Area | hya | Pi coding agent | Compat |
| --- | --- | --- | --- |
| Tool calling, MCP, skills | Unified Rust tool registry for built-ins, MCP, plugins, skills, subagents, todo, web, LSP, and formatter planes. MCP tools are `mcp__<server>__<tool>` and run through the normal permission/event path. | Minimal default tool core (`read`, `write`, `edit`, `bash`) with optional built-ins and extension-registered tools. Skills are first-class progressive-disclosure resources. MCP is not a documented stock-core headline feature. | Broad unified tool surface for built-ins, JS/TS custom tools, plugin tools, skills, and MCP tools. MCP is first-class, including local/remote servers and remote OAuth/DCR flows. |
| Multi-provider support | Configured routes for OpenAI-compatible, Anthropic, and Google/Gemini protocols, with capability preflight and an offline `DevProvider`. | Broad provider abstraction via `@earendil-works/pi-ai`, subscription auth, API-key providers, custom compatible models, and extension-provided providers. | Catalog-scale provider UX: docs claim 75+ providers plus local models, with many bundled provider adapters and provider-specific routing/auth behavior. |
| Multi-agent support | Native runtime primitives: `task` tool, child sessions, team evidence projection, mailbox/task board, and optional worktree allocation. | Stock runtime is intentionally single-agent/minimal-core; subagents and plan mode are documented as extension/SDK patterns, not baseline features. | Native primary agents and subagents, custom agent configs, per-agent permissions, and TUI child-session navigation. |
| TUI features | Terminal-first UI with current `hya-tui` command palette/status surfaces plus legacy slash-command flows and Compat-compatible routes. Permission/question overlays, session/model/status views, and backend-ready prompt queuing are implemented in current code. | Rich interactive TUI with file references, inline shell, model/settings/session/tree flows, queued steering/follow-up, external editor, images, themes, and extensionable UI components. | Rich TUI with file/reference autocomplete, slash commands, session sharing, Git-backed undo/redo, child-session navigation, remote attach, and configurable keybind/theme behavior. |
| Plugin/extensibility system | Native stdio plugin host plus Compat adapter. Plugins can add tools, hooks, permission interceptors, workspace adapters, and Compat-compatible plugin behavior. | TypeScript extensions and Pi packages are the main extension system: add tools, commands, providers, UI, hooks, prompts, skills, and themes. | Layered extensibility: standalone custom tools, plugins, commands, skills, references, provider/auth hooks, and MCP config. |

## Baseline and Caveats

- **hya baseline:** this repository's current architecture and docs, especially
  [Architecture Overview](architecture/overview.md),
  [Runtime](architecture/runtime.md),
  [Providers](architecture/providers.md),
  [Tools and Permissions](architecture/tools-and-permissions.md),
  [TUI](architecture/tui.md), [Configuration](configuration.md), and the
  Compat compatibility tracker [Compat Parity Matrix](compat-parity.md).
- **Pi baseline:** upstream stock Pi, primarily
  [`packages/coding-agent/README.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/README.md),
  [`docs/skills.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/skills.md),
  [`docs/extensions.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/extensions.md),
  [`docs/providers.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/providers.md),
  [`docs/models.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/models.md),
  [`docs/custom-provider.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/custom-provider.md),
  [`docs/packages.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/packages.md),
  and the package docs for
  [`@earendil-works/pi-ai`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/ai/README.md)
  and [`@earendil-works/pi-agent-core`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/agent/README.md).
- **Compat baseline:** current Compat docs at
  [compat.ai/docs](https://compat.ai/docs/) and the active
  [`anomalyco/compat`](https://github.com/anomalyco/compat) TypeScript
  codebase. The older [`compat-ai/compat`](https://github.com/compat-ai/compat)
  repository is archived and says the project continued as Crush; it is not used
  as the current feature baseline.
- **Known drift:** Compat docs list a built-in `scout` subagent, but the
  reviewed `packages/compat/src/agent/agent.ts` source confirmed `build`,
  `plan`, `general`, `explore`, plus hidden system agents. Treat `scout` as a
  docs/source drift note unless the upstream source changes.
- **hya TUI transition:** hya's current UI surface is `crates/hya` plus
  `crates/hya-tui`; the prior `hya-backend` + `hya-legacy-tui` renderer split
  has been removed. Compare hya's UI as the current frontend, not the deleted
  legacy renderer.

## 1. Tool Calling, MCP, and Skills

### hya

hya centralizes model-facing tools in `ToolRegistry`. A provider can request a
named tool call, but `SessionEngine` owns execution: it creates a `ToolCtx`,
checks permissions, executes the registered tool, records `ToolResult` or
`ToolError`, and starts another provider round if needed. That keeps built-ins,
MCP tools, plugin tools, and subagent tools on one replayable event path.

Core built-ins include:

- file tools: `read`, `write`, `edit`, `apply_patch`
- search/navigation: `ls`, `glob`, `find`, `grep`
- execution and interaction: `shell`/`bash`, `question`/`ask_user`
- higher-level planes: `lsp`, `skill`, `task`, `todowrite`, `plan_exit`
- web surfaces: `webfetch`, `websearch`

MCP servers are configured in `config.yaml`. Enabled servers start during
runtime composition; each exposed server tool is registered as
`mcp__<server>__<tool>` and uses the same permission plane as native tools. The
Compat-shaped HTTP MCP routes report status and accept compatibility add or
connect requests, but current docs explicitly say they do not durably rewrite
`config.yaml` or hot-plug new tools into an already-running engine.

Skills are first-class runtime content. Native runtime discovery reads hya skill
locations such as `.hya/skills` and user config skill directories, injects an
available-skill list into the prompt, and exposes a `skill` tool to load full
content on demand. The Compat-compatible server also scans Compat-style
skill locations for `/skill` and `/api/skill` catalog responses.

Evidence: [Tools and Permissions](architecture/tools-and-permissions.md),
[Runtime](architecture/runtime.md), [Configuration](configuration.md),
[`crates/hya-tool/src/tool.rs`](../crates/hya-tool/src/tool.rs),
[`crates/hya-mcp/src/bridge.rs`](../crates/hya-mcp/src/bridge.rs),
[`crates/hya-app/src/runtime.rs`](../crates/hya-app/src/runtime.rs), and
[`crates/hya-server/src/compat/skill_catalog.rs`](../crates/hya-server/src/compat/skill_catalog.rs).

### Pi coding agent

Stock Pi has a deliberately small default tool core. The coding-agent README
lists the default built-ins as `read`, `write`, `edit`, and `bash`; other stock
or extension-level tools such as `grep`, `find`, and `ls` exist, but they are not
the core headline. The agent loop supports streamed tool-calling turns and can
execute tools sequentially or in parallel through the underlying agent-core
package.

Pi skills follow the Agent Skills standard. At startup Pi discovers skill
metadata, advertises summaries to the model, and loads the full `SKILL.md` only
when the user invokes `/skill:name` or the model decides the task matches a
skill. This is similar to hya/Compat's progressive-disclosure approach, but
Pi's skill story is part of a broader package/extension ecosystem rather than a
Rust runtime plane.

MCP is not presented in the stock docs index as a default primitive beside
skills, extensions, providers, and custom models. Repo issues and extension docs
show MCP-adjacent integration paths, so the safest comparison is: stock Pi can
integrate external tools through extensions/packages, but MCP is not the central
stock-core abstraction in the way it is for hya and Compat.

Evidence: Pi
[`README.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/README.md),
[`docs/skills.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/skills.md),
[`docs/extensions.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/extensions.md),
and [`packages/agent/README.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/agent/README.md).

### Compat

Compat exposes a broad, unified tool plane. Built-ins, standalone JS/TS custom
tools, plugin-provided tools, and MCP-exposed tools are merged into one registry
and governed by permissions. Custom tools live in `.opencode/tools/` or the
global config directory, use the plugin SDK's `tool()` helper, can export
multiple tool definitions, and can override built-ins by name.

Skills are loaded through a native `skill` tool. Compat discovers skill
metadata from `.opencode`, `.claude/skills`, `.agents/skills`, and global
locations, then the model calls the `skill` tool to read full content.

MCP is a first-class Compat feature. Config supports local and remote servers,
MCP tools show up alongside built-ins, permissions can target MCP tool name
patterns, and remote MCP supports OAuth/Dynamic Client Registration plus CLI
auth/debug flows.

Evidence: Compat [tools](https://compat.ai/docs/tools),
[custom tools](https://compat.ai/docs/custom-tools),
[skills](https://compat.ai/docs/skills),
[MCP servers](https://compat.ai/docs/mcp-servers),
[`packages/compat/src/tool/registry.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/compat/src/tool/registry.ts),
and [`packages/plugin/src/tool.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/plugin/src/tool.ts).

## 2. Multi-Provider Support

### hya

hya supports multiple live protocol families through `hya-provider`:
OpenAI-compatible Chat Completions, Anthropic Messages, and Google/Gemini. Each
upstream is decoded into the same canonical event stream: text, reasoning, tool
input, tool-call requests, finish reasons, and errors.

`ProviderRouter` keeps ordered routes and resolves requests by model/capability.
`preflight` rejects tool-using requests if the selected route does not advertise
streaming tool-call support. Unsupported reasoning settings are stripped before
request dispatch. When no usable provider config exists, hya intentionally falls
back to `DevProvider`, an offline echo provider that keeps the app runnable
without API keys.

hya's provider story is strong on protocol normalization and local control, but
narrower than Pi/Compat in out-of-the-box provider breadth. The user supplies
provider routes in `~/.config/hya/config.yaml`; hya does not currently ship a
models.dev-scale catalog.

Evidence: [Providers](architecture/providers.md),
[Configuration](configuration.md),
[`crates/hya-provider/src/router.rs`](../crates/hya-provider/src/router.rs), and
[`crates/hya-provider/src/lib.rs`](../crates/hya-provider/src/lib.rs).

### Pi coding agent

Pi has broad provider support through `@earendil-works/pi-ai`. The docs describe
subscription-based auth for Claude Pro/Max, ChatGPT Plus/Pro Codex, and GitHub
Copilot, plus API-key providers and cloud providers. Compatible local or proxy
endpoints can be added through `models.json` when they speak a supported API;
nonstandard APIs, proxies, and custom OAuth flows use `pi.registerProvider()`
from an extension.

This gives Pi two layers of provider extensibility: built-in model/provider
routing for common vendors, and extension-level provider registration for custom
enterprise or local deployments.

Evidence: Pi [providers](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/providers.md),
[models](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/models.md),
[custom providers](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/custom-provider.md),
and [`packages/ai/README.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/ai/README.md).

### Compat

Compat has the broadest documented provider surface of the three. Official
docs claim 75+ providers and local models. Current source includes bundled
provider logic for Anthropic, OpenAI, OpenAI-compatible, Azure, Amazon Bedrock,
Google, Google Vertex, OpenRouter, xAI, Mistral, Groq, DeepInfra, Cerebras,
Cohere, TogetherAI, Perplexity, Vercel, Alibaba, GitLab, Venice, GitHub Copilot,
and Compat's own provider surface.

Compat's provider layer is not just a static list. Source has provider-specific
handling for auth, headers, base URLs, chat-vs-responses selection, Azure
resource naming, Bedrock region/profile/token behavior, and public fallback
paths. Config can shape per-provider model availability through `baseURL`,
`blacklist`, and `whitelist`; auth is managed by TUI `/connect` or CLI auth
commands.

Evidence: Compat [providers](https://compat.ai/docs/providers),
[CLI](https://compat.ai/docs/cli),
[`packages/compat/src/provider/provider.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/compat/src/provider/provider.ts),
and [`packages/core/package.json`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/core/package.json).

## 3. Multi-Agent Support

### hya

hya has native multi-agent runtime machinery. The `task` tool can launch one or
many child-session members and return bounded evidence to the lead session.
`run_team` and `run_member` keep child transcripts separate instead of dumping
full worker context into the parent. `TeamControlPlane` models lifecycle,
mailbox, and task-board state; `WorktreeManager` can allocate owned git
worktrees under `.hya/worktrees`.

Important constraints make this intentionally controlled rather than unbounded:
subagents cannot recursively spawn more subagents through `TaskTool`, and
background execution is constrained. The shipped CLI surfaces the main TUI,
headless runs, goal mode, server, replay, session, auth/catalog, and JSONL RPC;
the underlying team machinery is more developed than the end-user team UI.

Evidence: [Runtime](architecture/runtime.md),
[`crates/hya-tool/src/task.rs`](../crates/hya-tool/src/task.rs),
[`crates/hya-core/src/subagent.rs`](../crates/hya-core/src/subagent.rs),
[`crates/hya-core/src/team.rs`](../crates/hya-core/src/team.rs), and
[`crates/hya-core/src/workspace.rs`](../crates/hya-core/src/workspace.rs).

### Pi coding agent

Stock Pi is intentionally not a native multi-agent harness. Its README says the
core skips features such as subagents and plan mode. The built-in session model
focuses on continuation, branching, tree navigation, forking, cloning, and
message steering/follow-up queues.

Multi-agent behavior still exists as an extension/SDK pattern. The stock repo
ships example extensions for `subagent/` and `plan-mode/`; the subagent example
spawns separate `pi` processes with isolated context windows, and the SDK docs
explicitly mention custom tools that spawn subagents as a use case. That makes
Pi flexible, but the baseline product does not provide hya-style team control or
Compat-style first-class subagent sessions.

Evidence: Pi
[`README.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/README.md),
[SDK docs](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/sdk.md),
[extension examples](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/examples/extensions/README.md),
[subagent example](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/examples/extensions/subagent/index.ts),
and [plan-mode example](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/examples/extensions/plan-mode/index.ts).

### Compat

Compat has first-class primary agents and subagents. Built-in primary agents
include `build` and `plan`; source-confirmed subagents include `general` and
`explore`, with hidden system agents for compaction/title/summary. Agents can
have their own model, prompt, mode, and permission envelope. They can be defined
in JSON config or markdown files, auto-invoked, or manually mentioned from the
TUI.

The TUI exposes parent/child session navigation for subagent runs, which makes
multi-agent work visible to users. Compared with hya, Compat's subagent UX is
more polished and session-native, while hya's runtime primitives go deeper into
team control planes, mailbox/task-board state, and optional worktree allocation.

Evidence: Compat [agents](https://compat.ai/docs/agents),
[keybinds](https://compat.ai/docs/keybinds),
[CLI](https://compat.ai/docs/cli), and
[`packages/compat/src/agent/agent.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/compat/src/agent/agent.ts).

## 4. TUI Features

### hya

hya is terminal-first. The current frontend has the `hya` binary call
`hya_tui::app::run_tui`, create a pending SDK client, connect the backend off
the render path, fetch agents/MCP status, and queue prompts while the backend
starts. Current `hya-tui` runtime includes command palette entries for
session/model/agent/theme/status/editor/export/copy flows, session/home routing,
permission and question modals, model/provider status, MCP/LSP/formatter/plugin
status display, subagent status, toasts, slash commands, model switching,
session picking, transcript rendering, `/tools`, `/mcp`, `/export`, and
`/compact`.

Compat parity tracking shows hya implements some Compat-compatible TUI HTTP
control routes, but still lacks full Compat TUI parity such as command palette
depth, full theme picker/library, prompt stash, rich markdown/diff/code
rendering, usage/cost display wiring, and full leader-key UX.

Evidence: [TUI](architecture/tui.md),
[Compat Parity Matrix](compat-parity.md),
[`crates/hya/src/main.rs`](../crates/hya/src/main.rs), and
[`crates/hya-tui/src/app/runtime.rs`](../crates/hya-tui/src/app/runtime.rs).

### Pi coding agent

Pi's TUI is richer than a minimal chat REPL. It supports file-reference search
with `@`, path completion, multiline input, external editor launch, pasted or
dragged images, inline `!` and `!!` shell commands, model selection, scoped
model cycling, settings, resume/new session, tree navigation, fork/clone,
compaction, export/import/share, trust, reload, and hotkeys.

Pi also distinguishes steering from follow-up prompts while a run is active.
Extensions can replace the editor, add custom widgets, status/header/footer
content, overlays, shortcuts, and custom UI components. The monorepo ships a
standalone `@earendil-works/pi-tui` package, reinforcing that terminal UI is a
first-class platform layer rather than incidental CLI output.

Evidence: Pi
[`README.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/README.md),
[`docs/extensions.md`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/extensions.md),
and [`packages/coding-agent/src/modes/interactive/interactive-mode.ts`](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/src/modes/interactive/interactive-mode.ts).

### Compat

Compat's TUI emphasizes session ergonomics and control. It supports `@` fuzzy
file references, configured reference aliases, inline `!` shell execution,
slash commands, model/theme/session dialogs, thinking visibility, export,
external-editor composition, session sharing, and Git-backed undo/redo. Keybinds
are configurable in `tui.json`, including leader behavior, child-session
navigation, mouse behavior, diff style, scroll behavior, notifications, and
sounds.

Compat also exposes remote/control surfaces: CLI `attach`, server `/tui/*`
control routes, SDK `client.tui.*` methods, and PTY endpoints. That makes the
TUI programmable by IDE plugins and other integrations, not only directly used
by a human at a terminal.

Evidence: Compat [TUI](https://compat.ai/docs/tui),
[keybinds](https://compat.ai/docs/keybinds),
[share](https://compat.ai/docs/share),
[CLI](https://compat.ai/docs/cli),
[server](https://compat.ai/docs/server), and [SDK](https://compat.ai/docs/sdk).

## 5. Plugin System and Extending the Agent

### hya

hya's plugin host is a native stdio JSON-RPC system. Plugins can be configured in
`config.yaml` or discovered from `<workdir>/.hya/plugins/**/plugin.toml`.
Runtime composition connects configured plugins, registers declared tools into
the same registry as built-ins and MCP tools, installs hook dispatch, and can add
a permission interceptor. The host also tracks workspace-adapter metadata and
supervises plugin processes: crashes mark a plugin dead, later calls can respawn
it, and repeated failures disable it.

Adding a new hya tool through a plugin means declaring a plugin tool; once the
host connects, the tool is registered with `ToolRegistry` and the agent sees it
in the next tool schema set. Adding a skill means placing skill content in a hya
skill directory for native runtime discovery, or an Compat-style skill
directory when targeting the Compat-compatible server catalog. Letting the
agent communicate with MCP means adding an MCP server under `mcp:` in
`config.yaml`; hya starts the server, calls `tools/list`, wraps each tool as
`mcp__<server>__<tool>`, and later calls `tools/call` when the model invokes it.

hya also ships `crates/hya-plugin-compat`, a Bun/TypeScript adapter for
Compat plugins. That path gives hya compatibility with much of Compat's
plugin ecosystem while still feeding the Rust runtime's event, permission, and
tool planes.

Evidence: [Configuration](configuration.md),
[Tools and Permissions](architecture/tools-and-permissions.md),
[Compat adapter README](../crates/hya-plugin-compat/README.md),
[`crates/hya-plugin/src/host.rs`](../crates/hya-plugin/src/host.rs),
[`crates/hya-plugin/src/messages.rs`](../crates/hya-plugin/src/messages.rs),
[`crates/hya-plugin/src/permission_bridge.rs`](../crates/hya-plugin/src/permission_bridge.rs),
[`crates/hya-app/src/plugins.rs`](../crates/hya-app/src/plugins.rs), and
[`crates/hya-mcp/src/manager.rs`](../crates/hya-mcp/src/manager.rs).

### Pi coding agent

Pi's main extension system is TypeScript extensions plus Pi packages. An
extension can call `pi.registerTool()` to add a model-callable tool,
`pi.registerCommand()` to add slash commands, `pi.registerProvider()` to add
provider support, and `pi.on(...)` to handle lifecycle, prompt, provider,
compaction, tool, UI, and session events. Extensions live in global/project
extension directories or can be loaded from packages.

Pi packages bundle extensions, skills, prompts, and themes from npm, git, or
local paths. Project-local resources are gated by project trust. The agent knows
the skill list because Pi discovers skills from global/project/package/settings
sources, injects skill metadata into the prompt, and loads full content lazily.

For MCP-like external capabilities, stock Pi's documented route is extension or
package code: implement a custom tool, provider, or integration that talks to the
external service. That is powerful and flexible, but less standardized than
Compat's first-class MCP config or hya's `mcp:` runtime composition.

Evidence: Pi [extensions](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/extensions.md),
[packages](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/packages.md),
[skills](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/skills.md),
[custom providers](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/custom-provider.md),
and [README customization](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/README.md).

### Compat

Compat has several extension layers:

- standalone custom tools in `.opencode/tools/` or global tool directories
- plugins from local files or npm packages
- custom slash commands in `.opencode/commands/`
- markdown skills in Compat/Claude/Agents-compatible directories
- references to external directories or repositories
- MCP servers configured under `mcp`

Plugins can register tools and hook event/config/tool/auth/provider/chat/
permission/command/shell/tool-before-after surfaces, plus experimental workspace
adapters. Standalone custom tools use the same plugin SDK helper and can export
multiple tool definitions from one file.

The agent knows skills through the `skill` tool's available-skill inventory, and
it communicates with MCP by invoking MCP-exposed tool names through the regular
tool plane. Plugins can also use the Compat SDK client to interact with app,
MCP, auth, TUI, and other server surfaces where supported.

Evidence: Compat [plugins](https://compat.ai/docs/plugins),
[custom tools](https://compat.ai/docs/custom-tools),
[commands](https://compat.ai/docs/commands),
[skills](https://compat.ai/docs/skills),
[MCP servers](https://compat.ai/docs/mcp-servers),
[config](https://compat.ai/docs/config),
[`packages/plugin/src/index.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/plugin/src/index.ts),
[`packages/plugin/src/tool.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/plugin/src/tool.ts),
[`packages/compat/src/plugin/loader.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/compat/src/plugin/loader.ts),
and [`packages/compat/src/tool/registry.ts`](https://raw.githubusercontent.com/anomalyco/compat/dev/packages/compat/src/tool/registry.ts).

## Practical Takeaways

- Pick **hya** when you want a Rust event-sourced runtime with unified tool,
  permission, plugin, MCP, and multi-agent machinery, and when Compat API
  compatibility matters but exact upstream parity can be incremental.
- Pick **Pi** when you want a minimal, aggressively extensible terminal coding
  harness where workflow-specific features are built as TypeScript extensions or
  packages rather than mandated by the core.
- Pick **Compat** when you want the broadest out-of-the-box provider catalog,
  first-class MCP operations, rich TUI/session ergonomics, and a large
  config/file/plugin-centered extension surface.

No one tool is a strict superset on every axis. hya is strongest on event-sourced
runtime composition and native team primitives; Pi is strongest on minimal-core
customizability and extensionable TUI/provider workflows; Compat is strongest
on provider breadth, MCP operations, and polished agent/session UX.
