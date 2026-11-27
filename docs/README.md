# Hya Documentation

Hya is an AI coding assistant that runs locally in your terminal and in your browser, connects to any model provider you choose, and saves every conversation safely on your own machine.

Pick where you want to start based on what you're doing today:

---

## 1. Using Hya (For Everyone)

New to Hya, or looking up how to run a workflow, switch models, or connect from another machine? Start here.

### Get Started
- **[What is Hya?](getting-started/overview.md)** — How the terminal UI, browser WebUI, background server, and offline mode fit together.
- **[Installation](getting-started/installation.md)** — One-line install on Linux, macOS, and Windows (WSL 2), plus `hya update` and source builds.
- **[5-Minute Quickstart](getting-started/quickstart.md)** — Launch Hya, add an API key or OAuth login, edit code, approve prompts, and `/undo` changes.
- **[Common Workflows](getting-started/common-workflows.md)** — Copy-paste recipes for exploring code, fixing bugs, running on autopilot, and working remotely.

### Guides
- **[Interactive Mode (TUI & WebUI)](guide/interactive-mode.md)** — Layouts (`tui.toml` presets), `@file` & `!shell`, clipboard/browser file paste, `/undo` & `/fork`, and management screens.
- **[Editor Integration (ACP)](guide/editor-integration.md)** — Connecting Zed and other ACP v1 code editors directly to your local Hya daemon via `hya acp`.
- **[Headless & Automation](guide/headless-and-automation.md)** — Running Hya in scripts and CI with `hya exec`, `--bare`, `-p` Goal mode, and `hya loop`.
- **[Models, Providers & Auth](guide/models-and-providers.md)** — Connecting Anthropic, OpenAI, Gemini, Grok, or local models via `/key`, OAuth, or `config.yaml`.
- **[Projects, Permissions & Safety](guide/projects-and-permissions.md)** — Multi-folder Projects, outside-directory guards, `manual` vs `yolo` modes, and `/rules`.
- **[Context, Memory & Compaction](guide/context-and-memory.md)** — Teaching Hya project rules with `AGENTS.md` and how Hya shrinks long chats automatically at 80%.
- **[Subagents & Multi-Agent Teams](guide/subagents-and-teams.md)** — Built-in roles (`hya-main`, `hya-plan`, `hya-task`, `hya-scout`, `hya-reviewer`), `/subagents`, and vertical channels.
- **[Workflows](guide/workflows.md)** — Running multi-stage `*.hya.md` pipelines from `/workflow` or the CLI.
- **[Skills, Custom Commands & MCP](guide/skills-commands-and-mcp.md)** — Loading `SKILL.md` guides, writing `/commands`, connecting MCP tool servers, and Code Mode.
- **[Background Server & Remote Relay](guide/daemon-and-relay.md)** — Controlling `hya serve` and connecting across machines over end-to-end encrypted `hya://` links.

### Reference Manuals
- **[CLI Reference](reference/cli.md)** — Every `hya` command, flag, database lock rule, and exit code.
- **[Configuration Reference](reference/configuration.md)** — Complete field dictionary for `~/.config/hya/config.yaml` and `~/.config/hya/tui.toml`.
- **[Slash Commands & Keybindings](reference/commands-and-keybindings.md)** — Every `/command`, keyboard shortcut, and `/keybind` customization.
- **[Built-in Tools, Agents & Bundles](reference/tools-and-presets.md)** — All 6 Kernel tools, 25 Registry tools, built-in agents, 13 `bundles/builtin/` packages, and `hya-extra/*` bundles.
- **[Environment Variables & File Paths](reference/env-and-paths.md)** — Every `HYA_*` variable and XDG file location on disk.
- **[Troubleshooting](reference/troubleshooting.md)** — Quick fixes for busy ports, daemon locks, `403` host errors, offline mode, and relay setup.

---

## 2. Extending & Integrating Hya (For Plugin & App Developers)

Customize Hya or build your own apps and scripts on top of it—without touching Hya's core Rust source code.

- **[Extending Hya: Overview](extend/overview.md)** — Decision table: `AGENTS.md` vs Skills vs Commands vs Workflows vs MCP vs `.hyabundle` vs TUI Extensions.
- **[Authoring Skills & Slash Commands](extend/skills-and-commands.md)** — `SKILL.md` frontmatter, discovery precedence, and `.hya/commands/*.md` argument templates.
- **[Authoring Workflows & Evaluators](extend/authoring-workflows.md)** — Writing `*.hya.md` Stage DAGs, loop verifiers, per-stage model routing, and custom Goal/Loop judges.
- **[Creating Bundles (`.hyabundle`)](extend/bundles.md)** — Packaging agents, skills, tools, custom HTTP APIs (`apis:`), and permission modes—plus importing Claude Code plugins (`--claude`).
- **[`hyabundle` Specification](extend/hyabundle-spec.md)** — Canonical `bundle.yaml` & descriptor schemas, JS vs Native ABI contracts, `bundles/builtin/` layout, loading precedence, and independent Bundle CI.
- **[Sidecar Plugins, Custom Tools & Hooks](extend/plugins-and-hooks.md)** — Writing Bun JS modules or NDJSON JSON-RPC 2.0 plugins, `host/capability` calls, and all 19 hooks.
- **[TUI Extensions (`@hya/tui-sdk`)](extend/tui-extensions.md)** — Building sandboxed custom UI panels, status items, renderers, and prompt interceptors.
- **[`hya.v1` Protocol Overview](protocol/README.md)** · **[SDK & Client Integration](protocol/sdk.md)** · **[Generated API Reference](protocol/api-reference.md)** · **[OpenAPI Schema](protocol/openapi.json)** — Driving Hya over HTTP/JSON+SSE+WebSocket, gRPC, or Rust `hya-sdk-v1`.

---

## 3. Architecture & Contributing (For Core Contributors)

Working inside the `hya` repository or exploring how the runtime is built?

- **[Architecture Overview](architecture/overview.md)** — Event-sourced truth, 3-primitive Kernel vs Full Mode, persistent daemon, single-port `hya.v1`, and system diagrams.
- **[Repository & Crate Map](architecture/codebase-map.md)** — Workspace crates, native tool bundles, frontend packages, and where to make a change.
- **[Kernel & Context Engine](architecture/kernel-and-context.md)** · **[Event Model & Storage](architecture/events-and-storage.md)** · **[Session Engine, Subagents & Channels](architecture/engine-and-subagents.md)** · **[Providers, Tool Plane & Scoping](architecture/providers-and-tools.md)** · **[Server, Relay & Frontend Internals](architecture/server-relay-and-tui.md)** · **[Domain Glossary](architecture/glossary.md)**
- **[Development & Coding Standards](contributing/development.md)** · **[Plan Template](templates/plan.md)** · **[Testing & Verification](contributing/testing.md)** · **[Dev Tooling (`xtask`) & Docs Site](contributing/tooling-and-docs-site.md)** · **[Versioning & Release Guide](contributing/release.md)**
- **[Architecture Decision Records (`docs/adr/`)](adr/0001-event-sourced-mailbox-and-channels.md)**
