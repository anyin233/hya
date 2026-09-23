# Skills

Author, discover, and use `SKILL.md` skills in hya. Skills are on-demand
markdown bodies the model can load through the `skill` tool. The catalog is
built from directory discovery plus two first-party fallback skills.

Sources:
[`crates/hya-tool/src/skill_catalog.rs`](../crates/hya-tool/src/skill_catalog.rs),
[`crates/hya-server/src/support/skill_catalog.rs`](../crates/hya-server/src/support/skill_catalog.rs).

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
| `allowed-tools` | no | List of tool name strings. **Parsed and stored**, and hashed into the skill-view semantic-identity digest only. **Not enforced** as a runtime tool allowlist — declaring `allowed-tools: [read]` does not restrict which tools the model may call. |
| `model` | no | Optional model string. **Parsed and stored**, and hashed into the skill-view semantic-identity digest only. **Not used** for model routing or turn completion. |
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

The trusted `hya/core-skills` Plugin owns the two built-in Skill files under
`bundles/presets/core-skills/resources/skills/`. The bundle is loaded from its
[first-party source](bundle-runtime.md#first-party-bundles) at startup, and
its declared resources populate the catalog. Discovery and
captured Skill execution append these entries only when no discovered Skill of
the same name exists:

| Name | Purpose (summary) |
| --- | --- |
| `agent-bundle-authoring` | Authoring and packaging public AgentBundles (static or Bun sidecars). |
| `secure-self-update` | Verifying, staging, and owner-activating independent hya releases via `hya-updater`. |

A user-authored skill with a matching `name` **shadows the built-in entirely**.

Both `/skill` and skill-backed `/command` entries use this effective catalog,
as do captured Session calls to the `skill` tool. Skills from `hya/core-skills`
(`SkillCatalogOrigin::Embedded`) have no filesystem base directory or sampled
file list: load them with `skill`, not by opening their synthetic catalog
path. Existing tool output limits still apply, so a long skill body can be
truncated like any other skill output.

For example, `hya bundle info hya/core-skills` lists both Skill ids, and a
model can call `skill` with `{"name":"agent-bundle-authoring"}`. To change a
built-in Skill, edit its `SKILL.md` in the bundle source; a Cargo build picks
up the change on the next restart with no rebuild. No public installation or
runtime file scan is involved. `hya/core-skills` is an immutable,
noninstallable trusted inventory entry.

The bundle's `bundle.yaml` declares `kind: Plugin`, identity
`hya/core-skills` version `1.0.0`, and two `resources.skills` entries with
`id` and `path`. Each `SKILL.md` begins with YAML frontmatter containing
`name: string` and `description: string`, then the Markdown body. The `name`
must equal the declared resource `id`; invalid or missing metadata fails to
load at startup. `hya_tool::core_skills_preset_bytes()` returns the loaded
bundle's exact prepared catalog bytes for inventory and audit. The runtime
Skill catalog exposes
`name`, `description`, body `content`, empty `allowed_tools`, no model override,
`SkillCatalogOrigin::Embedded`, and a synthetic path rooted at
`embedded:hya/core-skills/skill/`.

---

## How skills surface to the model

Discovered skills contribute a system-prompt section of the form “these skills
are available on demand; read the named SKILL.md when relevant”, listing each
`name: description`. The body is loaded when the model invokes the `skill` tool
(or equivalent). There is no per-skill tool gate from `allowed-tools` at that
point.

When an agent’s resource view selects harness skills, it must also select the
`skill` tool facade; otherwise the view is rejected. See
[AgentBundle authoring](agent-bundle-authoring.md#resource_view).

---

## Related

- [AgentBundle authoring](agent-bundle-authoring.md) — `resources.skills` inside a bundle package
- [Configuration](configuration.md) — pointer to this guide for skill discovery
- [Plugin protocol](plugin-protocol.md) — unrelated to skill files; for process plugins
