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
| `disable-model-invocation` | no | When `true` (as in pi and the Agent Skills specification), the skill stays out of the model's skill index (the recorded directory index and the bundle skill section), so the model does not pick it on its own; `/skill:<name>` still runs it, `/skills` lists it (`only /skill:<name>`), and `ListSkills` reports `disableModelInvocation: true`. Default `false`. |
| `license` | no | Parsed but **currently unused** by the runtime. |

Every field beyond `name` / `description` is optional so minimal skills keep
working.

### Naming rules

Names follow the [Agent Skills](https://agentskills.io/specification) rule:
lowercase letters, digits, and hyphens, with no leading, trailing, or doubled
hyphen, at most 64 characters; descriptions at most 1024 characters. A skill
that breaks either rule is still listed and runs, with a warning (below).
Matching the name to the skill's directory is the portable choice; hya does
not require it.

### Skipped files and warnings

Discovery reports every `SKILL.md` it skips or warns about, as pi does, in
`ListSkillsResponse.diagnostics` (`CatalogDiagnostic {path, kind, message}`),
in the TUI's startup list under `[Skills]` (in the warning color, also with
`quiet = true`), and at the top of the [Skills view](tui.md#skills-view):

| `kind` | When | Skill |
| --- | --- | --- |
| `invalid` | The file does not start with `---`, the closing fence is missing, or the YAML does not parse | Skipped |
| `missing_name` | No `name` | Skipped |
| `missing_description` | No `description` | Skipped |
| `collision` | A skill of the same name was found first (the message names that file) | Skipped |
| `name` | The name breaks the naming rule | Listed |
| `description_length` | The description is over 1024 characters | Listed |

`disable: true` is deliberate and is not reported. A directory without a
readable `SKILL.md` is not a skill and is not reported either. For example:

```text
[Skills] agent-bundle-authoring, pdf-tools, secure-self-update
  ~/.codex/skills/notes/SKILL.md: no `description`; skipped · /skills
```

---

## Discovery search path (first name wins)

When HOME is set, hya walks these roots **in order**
([`skill_dirs_for_workdir`](../crates/hya-tool/src/skill_catalog.rs)):

1. `<workdir>/.hya/skills`
2. `$HOME/.config/hya/skills`
3. `$HOME/.claude/skills`
4. `<workdir>/.agents/skills`
5. `$HOME/.codex/skills`
6. `$HOME/.agents/skills`

HOME-based entries are omitted when `HOME` is unset. Within each root, immediate
subdirectories are sorted by path, then each `SKILL.md` is parsed.

**First occurrence of a given skill `name` wins.** Later directories cannot
override an earlier skill of the same name (`HashSet` insert on name).

### Multi-root (Project) discovery order

[`skill_dirs_for(workdir, roots)`](../crates/hya-tool/src/skill_catalog.rs) and
[`discover_skills_for_roots_with_builtins`](../crates/hya-tool/src/skill_catalog.rs)
extend the same first-name-wins rule across a Project's additional roots. The
engine's per-scope binding composes its Skill catalog through
`discover_skills_for_roots_with_builtins`, so this is the order in effect:

1. `<workdir>/.hya/skills`
2. `<root>/.hya/skills` for each Project root, in root order
3. the user directories (`$HOME/.config/hya/skills`, `$HOME/.claude/skills`, …)
4. `<workdir>/.agents/skills`
5. `<root>/.agents/skills` for each Project root, in root order
6. `$HOME/.codex/skills`, `$HOME/.agents/skills` as today

Directories are deduped (e.g. a root equal to `workdir` is only scanned once).
As above, the first occurrence of a given skill `name` wins.

---

## Bundle and plugin skills

A bundle (`resources.skills`) or plugin can contribute skills too. They are
selected per agent: an agent sees the bundle skills its resource view
selects (a bundle agent only its own). `ListSkills` and `ListCommands` with a
`session` list what that session's agent can load — its directory skills, the
built-in ones, and its bundle skills (`source: "bundle"`, `bundleId:
"<publisher>/<name>"` or `"plugin:<id>"`, `location: "bundle:…"`) — and the
TUI asks with the open session, so `/skills` and the command menu show them
(`/skill:<name>`, grouped under `bundle <id>`). A bundle skill has no
directory on disk, so its `<skill>` block has no `References are relative
to` line. Without a session the listing is the directory's: no bundle skills.

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
| `secure-self-update` | Installing and updating unified hya releases via `hya update` (embedded installer, atomic rollback), and the source-checkout rebuild-and-restart flow. |

A user-authored skill with a matching `name` **shadows the built-in entirely**.

`/skill:<name>` commands use this effective catalog, as do captured Session
calls to the `skill` tool. Skills from `hya/core-skills`
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

The bundle's `bundle.yaml` declares `kind: Bundle`, identity
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

## Invoke a skill

Run a skill yourself with `/skill:<name> [request]`, the same command name pi
uses. The model may fail to load a skill that fits; the command forces it.

```text
/skill:pdf-tools extract report.pdf
```

The backend command catalog lists every visible skill as one row named
`skill:<name>` (`GET /v1/commands`: `source: "skill"`, `argumentHint:
"[request]"`, the skill body as `template`). `skill:` is a reserved namespace:
a command file named `skill:<x>.md` is skipped, so a skill and a command of
the same name never shadow each other — `/review` runs the command and
`/skill:review` the skill.

A command turn for `skill:<name>` (`CreateTurn` with `{command: {command:
"skill:<name>", arguments}}`) sends this text as the user message:

```text
<skill name="pdf-tools" location="/work/.hya/skills/pdf-tools/SKILL.md">
References are relative to /work/.hya/skills/pdf-tools.

<the SKILL.md body, without its frontmatter>
</skill>

extract report.pdf
```

- The request is appended after a blank line, as the user's ask; it is never
  substituted into the body (`$ARGUMENTS` in a body stays as written). With
  no request the block ends the message.
- `location` is the skill file; a built-in skill names its synthetic path
  (`embedded:hya/core-skills/skill/<name>/SKILL.md`) and has no `References
  are relative to` line, since it has no directory. Attribute values escape
  `&`, `"`, `<`, and `>` as `&amp;`, `&quot;`, `&lt;`, and `&gt;`.
- The block is expanded server-side from the session's catalog (its workdir,
  then its Project's roots), so a skill added or edited on disk is used by
  the next command without a restart.

The TUI folds the block in the transcript to one line and shows the request
below it, in every client (it reads the durable message text):

```text
┃ ▸ /skill:pdf-tools · 3 lines
┃ extract report.pdf
```

Click the line to show the whole block. An unknown `/skill:<name>` is refused
in the TUI (`Unknown skill <name> · /skills lists them`) and never reaches the
model. Bare `/<name>` no longer runs a skill: the TUI answers `Unknown command
/<name> · did you mean /skill:<name>?`.

A plugin's `command.execute.before` hook sees `command: "skill:<name>"` and the
expanded block as `text` ([Plugin protocol](plugin-protocol.md)).

---

## How skills surface to the model

At its first model turn, a full-mode session records the directory skill
index visible to its agent. The index lists each `name: description`, including
the built-in fallback entries. Later requests reuse the recorded list. To put
an added, removed, or edited skill into a running session's index, run
`/reload-skills` (or `/reload-context`, `RefreshSessionContext`): the next
turn records the index again (see [Browse and reload skills in the
TUI](#browse-and-reload-skills-in-the-tui)); a new session works too. The
`skill` tool and `/skill:<name>` always use the current catalog, so they load
an edited body without a reload. There is no per-skill tool
gate from `allowed-tools` at that point.

When an agent’s resource view selects harness skills, it must also select the
`skill` tool facade; otherwise the view is rejected. See
[AgentBundle authoring](agent-bundle-authoring.md#resource_view).

---

## Browse and reload skills in the TUI

- `/skills [filter]` opens the [Skills view](tui.md#skills-view): every skill
  of the scope under its source directory, with its file, description, and
  body; `u` puts `/skill:<name> ` into the message input.
- `/reload-skills` re-reads the catalog (the command menu's `/skill:*` rows,
  the startup list's `[Skills]`, the Skills view) and, with a session open,
  re-records its skill index at its next turn. `/reload` does that and
  reloads everything else too (see [Reload](tui.md#reload)).

For example, after adding `.hya/skills/pdf-tools/SKILL.md` while the TUI
runs:

```text
/reload-skills
Reloaded 4 skills, context (re-recorded at the next turn)
/skill:pdf-tools extract report.pdf
```

---

## Related

- [AgentBundle authoring](agent-bundle-authoring.md) — `resources.skills` inside a bundle package
- [Configuration](configuration.md) — pointer to this guide for skill discovery
- [Plugin protocol](plugin-protocol.md) — unrelated to skill files; for process plugins
