# Skills

Author, discover, and use `SKILL.md` skills in hya. Skills are on-demand
markdown bodies the model can load through the `skill` tool. The catalog is
built from directory discovery plus three server-side built-in fallbacks.

Sources:
[`crates/hya-tool/src/skill_catalog.rs`](../crates/hya-tool/src/skill_catalog.rs),
[`crates/hya-server/src/compat/skill_catalog.rs`](../crates/hya-server/src/compat/skill_catalog.rs).

Bundle-local skill resources (`resources.skills`) are separate: they live inside
an AgentBundle package. See [AgentBundle authoring](agent-bundle-authoring.md).

---

## File layout

A skill is a directory that contains exactly one `SKILL.md`:

```text
my-skill/
└── SKILL.md
```

Discovery scans only **immediate subdirectories** of each skill root. Nested
trees are not walked. A directory without a readable `SKILL.md` is skipped.

---

## `SKILL.md` format

The file **must** begin with a leading `---` fence. YAML frontmatter is parsed
with `serde_norway` (YAML). Everything after the closing `---` fence is the
skill body (the content loaded on demand).

```markdown
---
name: reviewer
description: Reviews code for correctness and style.
allowed-tools:
  - read
  - grep
model: anthropic/claude-sonnet-4-6
license: MIT
---

# Reviewer skill

When invoked, inspect the named paths and report findings.
```

### Frontmatter fields

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | **yes** | Skill id shown to the model and used for first-name-wins. |
| `description` | **yes** | Short summary injected into the available-skills prompt section before the body is loaded. |
| `allowed-tools` | no | Per-skill tool allowlist (list of strings). **Empty or absent means unrestricted.** |
| `model` | no | Optional per-skill model override. |
| `disable` | no | When `true`, the skill is skipped entirely (never appears in the catalog). Default `false`. |
| `license` | no | Parsed but **currently unused** by the runtime. |

Every field beyond `name` / `description` is optional so minimal skills keep
working.

### Silent skip (most common authoring mistake)

`parse_skill` returns `None` (and discovery **silently skips** the skill with
**no error**) when:

- the file does not start with `---`
- the closing frontmatter fence is missing
- YAML fails to parse
- `name` or `description` is missing
- `disable: true`

If a skill “does not appear”, check name/description and the frontmatter fence
first.

---

## Discovery search path (first name wins)

When HOME is set, hya walks these roots **in order**
([`skill_dirs_for_workdir`](../crates/hya-tool/src/skill_catalog.rs)):

1. `<workdir>/.hya/skills`
2. `$HOME/.config/hya/skills`
3. `$HOME/.claude/skills`
4. `$HOME/.config/opencode/skills`
5. `$HOME/.config/opencode/skill` (singular)
6. `<workdir>/.opencode/skills`
7. `<workdir>/.opencode/skill` (singular)
8. `<workdir>/.agents/skills`
9. `$HOME/.codex/skills`
10. `$HOME/.agents/skills`

HOME-based entries are omitted when `HOME` is unset. Within each root, immediate
subdirectories are sorted by path, then each `SKILL.md` is parsed.

**First occurrence of a given skill `name` wins.** Later directories cannot
override an earlier skill of the same name (`HashSet` insert on name).

Both the singular `skill` and plural `skills` spellings are scanned for the
OpenCode-style roots.

---

## Built-in fallback skills

The server’s skill listing (`hya-server` Compat skill catalog) starts from
directory discovery, then **appends** three embedded templates with location
`"<built-in>"` **only when** no discovered skill of the same name exists:

| Name | Purpose (summary) |
| --- | --- |
| `customize-compat` | Editing or creating Compat’s own configuration (`opencode.json`, `.opencode/`, plugins, MCP, permission rules). |
| `agent-bundle-authoring` | Authoring and packaging public AgentBundles (static or Bun sidecars). |
| `secure-self-update` | Verifying, staging, and owner-activating independent hya releases via `hya-updater`. |

A user-authored skill with a matching `name` **shadows the built-in entirely**.

---

## How skills surface to the model

Discovered skills contribute a system-prompt section of the form “these skills
are available on demand; read the named SKILL.md when relevant”, listing each
`name: description`. The body is loaded when the model invokes the `skill` tool
(or equivalent), subject to any `allowed-tools` policy on that skill.

When an agent’s resource view selects harness skills, it must also select the
`skill` tool facade; otherwise the view is rejected. See
[AgentBundle authoring](agent-bundle-authoring.md#resource_view).

---

## Related

- [AgentBundle authoring](agent-bundle-authoring.md) — `resources.skills` inside a bundle package
- [Configuration](configuration.md) — pointer to this guide for skill discovery
- [Plugin protocol](plugin-protocol.md) — unrelated to skill files; for process plugins
