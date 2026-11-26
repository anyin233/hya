# Extending Hya: Overview

You can customize almost every part of Hya—its instructions, slash commands, tools, subagents, multi-step workflows, approval logic, and terminal UI—**without forking or recompiling Hya**.

Use this page to pick the lightest tool that solves your problem.

---

## Which extension mechanism should I pick?

| What you want to do | Reach for | Code required? | Where it lives |
| --- | --- | --- | --- |
| Teach Hya rules that apply to **every** chat in a repo | **[`AGENTS.md`](../guide/context-and-memory.md)** | None (Markdown) | `<repo>/AGENTS.md` or `~/.config/hya/AGENTS.md` |
| Give Hya domain knowledge loaded **only when needed** | **[Skills](skills-and-commands.md)** | None (Markdown) | `.hya/skills/<name>/SKILL.md` |
| Save a reusable prompt you trigger with `/name` | **[Custom Commands](skills-and-commands.md#2-custom-slash-commands)** | None (Markdown) | `.hya/commands/<name>.md` |
| Connect existing external tools (GitHub, DBs, browsers) | **[MCP Servers](../guide/skills-commands-and-mcp.md#3-mcp-servers-model-context-protocol)** | None (`config.yaml`) | `~/.config/hya/config.yaml` `mcp:` |
| Run a repeatable multi-agent pipeline (plan → code → review) | **[Workflows](authoring-workflows.md)** | None (`*.hya.md` + `flowchart TD`) | `.hya/workflows/*.hya.md` or inside a Bundle |
| Ship custom Agents, Tools, Hooks, APIs, or Claude plugins as one package | **[Bundles (`.hyabundle`)](bundles.md)** | YAML + optional JS/TS or Rust | `~/.local/share/hya/bundles/` or `.hya/bundles/` |
| Intercept tool calls, rewrite model requests, or add custom approval modes | **[Plugins & Hooks](plugins-and-hooks.md)** | TypeScript (Bun) or any stdio JSON-RPC binary | Inside a Bundle or `config.yaml` `plugins:` |
| Add custom UI panels, status items, or tool-card renderers to the TUI | **[TUI Extensions](tui-extensions.md)** | TypeScript (`@hya/tui-sdk`) | `tui:` section inside a Bundle |
| Embed or drive Hya from your own web app, bot, or CLI | **[`hya.v1` API & SDK](../protocol/README.md)** | HTTP/JSON+SSE, gRPC, or Rust `hya-sdk-v1` | Your own client process |

---

## Start small, grow when needed

1. **Start with Markdown.** 80% of team customizations need nothing more than a 50-line `AGENTS.md`, two or three `.hya/commands/*.md` shortcuts, and a `SKILL.md` for tricky subsystems.
2. **Step up to Workflows (`*.hya.md`)** when you want Hya to follow an explicit multi-stage graph where a reviewer verifies an implementer's work instead of letting one agent grade its own homework.
3. **Package a `.hyabundle`** when you want to distribute custom agents, Bun/Rust tools, lifecycle hooks, custom HTTP endpoints, or TUI panels to teammates in a single verified file (`hya bundle install team-pack.hyabundle`).

Already built plugins for **Claude Code**? You don't have to rewrite them—install them straight into Hya with:

```sh
hya bundle install --claude ./my-claude-plugin
```

See **[Creating Bundles](bundles.md#importing-claude-code-plugins---claude)** for details.
