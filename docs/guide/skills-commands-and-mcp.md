# Skills, Custom Commands & MCP

Out of the box, Hya knows how to read files, edit code, search the web, and run shell commands. When you want to give it specialized know-how, reusable prompt shortcuts, or outside tools (like GitHub, Linear, Slack, or Postgres), you have three lightweight options—no coding required.

---

## 1. Skills (`SKILL.md`)

A **Skill** is a folder containing a `SKILL.md` instruction guide. Unlike `AGENTS.md` (which is loaded into every single chat), Skills stay out of the way until they are needed: Hya sees only a one-line `name: description` summary at startup, and loads the full `SKILL.md` instructions **only when a task calls for it** (or when you invoke it yourself).

### Compatible out of the box

If you already have skills from Claude Code, Codex, or Pi, Hya finds them automatically! It checks these folders in order (first match wins):

1. `<project>/.hya/skills/<name>/SKILL.md`
2. `~/.config/hya/skills/<name>/SKILL.md`
3. `~/.claude/skills/<name>/SKILL.md`
4. `<project>/.agents/skills/<name>/SKILL.md`
5. `~/.codex/skills/<name>/SKILL.md`
6. `~/.agents/skills/<name>/SKILL.md`

### Browsing and running Skills

- **`/skills`** opens a full-screen browser showing every Skill found on your machine, where it came from, and any syntax warnings.
- **`/skill:<name> <your request>`** forces Hya to load that Skill right now for your message:
  ```text
  /skill:agent-bundle-authoring Help me package a read-only SQL helper bundle
  ```
- Added or edited a `SKILL.md` while Hya was already open? Run **`/reload-skills`**.

---

## 2. Custom Slash Commands

Have a prompt you type ten times a week—like *"Review staged git changes for bugs"* or *"Write release notes since the last tag"*? Save it as a Markdown file and run it with `/your-command`!

Create `.hya/commands/check-pr.md` in your project (or `~/.config/hya/commands/check-pr.md` for all projects):

```markdown
---
description: Review current branch changes before opening a PR
---
Run `git diff main...HEAD`, check every changed file for edge cases or missing tests, and give me a short checklist of anything to fix before merging.

Extra focus: $ARGUMENTS
```

Now inside Hya (run `/reload-commands` if Hya is already open), just type:

```text
/check-pr check SQL injection and null handling
```

Hya also includes handy built-in commands like **`/init`** (drafts an `AGENTS.md` for your repo) and **`/review`** (reviews your working tree changes).

---

## 3. MCP Servers (Model Context Protocol)

**MCP** lets Hya plug into local stdio tool servers or remote HTTP MCP servers—database inspectors, browser automation, issue trackers, documentation search, and more.

Add MCP servers under `mcp:` in `~/.config/hya/config.yaml`:

```yaml
mcp:
  memory:
    command: ["npx", "-y", "@modelcontextprotocol/server-memory"]

  github:
    command: ["npx", "-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_PERSONAL_ACCESS_TOKEN: "{env:GITHUB_TOKEN}"

  docs-remote:
    url: https://mcp.example.com/mcp
```

Every tool from an MCP server appears to Hya as `mcp__<server>__<tool>` (for example `mcp__github__list_issues`) and goes through Hya's normal permission checks.

### Managing MCP servers inside Hya (`/mcp`)

Type **`/mcp`** in the TUI to see every configured server:
- Press **`Enter`** on a server to inspect all the tools it exposes.
- Press **`c`** to connect or **`x`** to disconnect a server live.
- Press **`a`** if a server asks for OAuth login.

---

## 4. Optional: Code Mode (`tools.code.mode`)

When Hya needs to inspect 20 files or run several tools back-to-back, calling one tool per round can feel slow. Turning on **Code Mode** gives built-in agents an extra `code-mode__run` tool that lets the model write a small JavaScript snippet to call multiple Hya tools in parallel, filter big outputs, and return just the summary in a single round!

Enable it in `~/.config/hya/config.yaml` (then run `hya serve restart`):

```yaml
tools:
  code:
    mode: with_tools   # normal (off) | with_tools | only
```

Every tool call inside a Code Mode script still obeys your exact same Project boundaries and permission prompts.

Want to write your own Skills, Commands, or full Bundles? See **[Extending Hya](../extend/overview.md)**.
