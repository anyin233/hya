# Authoring Skills & Slash Commands

Skills and custom slash commands are plain Markdown files. You drop them on disk, run `/reload` (or start a session), and they work right away.

---

## 1. Authoring Skills (`SKILL.md`)

A **Skill** is a directory containing a single `SKILL.md` file:

```text
.hya/skills/
└── db-migrations/
    ├── SKILL.md
    └── checklist.sql     # optional helper files the skill tells Hya to read
```

Hya scans only **immediate subdirectories** of each skill root. At the start of a chat, Hya puts only each skill's `name` and `description` into the model's index—keeping your context window lean. The full Markdown body is loaded only when the model calls the `skill` tool or when you run `/skill:<name>`.

### `SKILL.md` format

Every `SKILL.md` **must** start with a `---` YAML frontmatter block containing at least `name` and `description`:

```markdown
---
name: db-migrations
description: Safe PostgreSQL schema migration rules and rollback checklist. Use whenever adding or altering database tables.
disable-model-invocation: false
---

# Database Migration Checklist

1. Never rename or drop an existing column in the same deploy that stops reading it.
2. Read `checklist.sql` in this skill folder before writing `ALTER TABLE` statements.
3. Run `cargo sqlx prepare --check` after editing queries.
```

### Frontmatter reference

| Field | Required | Default | Behavior |
| --- | --- | --- | --- |
| `name` | **Yes** | — | Lowercase letters, digits, and single hyphens (`a-z0-9-`, max 64 chars). Matches Agent Skills spec. |
| `description` | **Yes** | — | Up to 1,024 chars explaining *what* the skill covers and *when* the model should load it. |
| `disable` | No | `false` | When `true`, Hya skips the skill completely. |
| `disable-model-invocation` | No | `false` | When `true`, hides the skill from the model's auto-discovery list so it runs *only* when a user types `/skill:<name>` manually. |
| `allowed-tools`, `model`, `license` | No | — | Accepted for compatibility with Claude/Agent Skills files; stored in catalog metadata (not enforced as runtime tool/model overrides). |

### Discovery order (first name wins)

When two directories define a skill with the same `name`, the **first** one found wins:

1. `<workdir>/.hya/skills` (then each additional root in your Project, in root order)
2. `~/.config/hya/skills`
3. `~/.claude/skills`
4. `<workdir>/.agents/skills` (then each additional Project root)
5. `~/.codex/skills`
6. `~/.agents/skills`
7. Built-in fallback skills (`hya/core-skills`: `agent-bundle-authoring` and `secure-self-update`)

### How `/skill:<name>` expands on the wire

When a user runs `/skill:db-migrations add an index on users.email`, Hya expands it server-side into a structured `<skill>` block followed by the user's request (and folds it neatly to one line in the TUI):

```xml
<skill name="db-migrations" location="/work/.hya/skills/db-migrations/SKILL.md">
References are relative to /work/.hya/skills/db-migrations.

# Database Migration Checklist
...
</skill>

add an index on users.email
```

### Checking for syntax errors

If a `SKILL.md` is missing its `---` fence, missing `name`/`description`, or shadowed by an earlier folder, Hya never fails silently—it highlights the warning in the startup list and at the top of **`/skills`**. After editing a skill on disk, run **`/reload-skills`** to update your open session.

---

## 2. Custom Slash Commands (`commands/*.md`)

Custom commands turn Markdown prompt templates into `/command` shortcuts in the TUI.

### Where to put command files

Hya walks these folders **recursively** (first match wins):

1. **Project commands:** `<workdir>/.hya/commands/**/*.md` (and `.hya/command/**/*.md`), then each root of the active Project.
2. **User commands:** `~/.config/hya/commands/**/*.md` (and `~/.config/hya/command/**/*.md`).

Nested folders become `/` names: `.hya/commands/git/release-notes.md` becomes **`/git/release-notes`**. *(Names starting with `skill:` are reserved for skills.)*

### Command file format

Frontmatter is optional, and the body supports positional arguments (`$1`, `$2`, `$ARGUMENTS`) with shell-style defaults:

```markdown
---
description: Write unit tests for a file
argument-hint: "<file> [edge-cases]"
---
Write thorough unit tests for @$1.

Focus areas: ${2:-happy path, boundary values, and error cases}.
Run the test suite after writing them and fix any failures.
```

Try it in Hya:

```text
/reload-commands
/write-tests src/parser.rs "malformed UTF-8 and empty inputs"
```

### Argument substitution rules

| Placeholder | Meaning |
| --- | --- |
| `$1`, `$2`, … | Positional argument (quoted strings like `"foo bar"` count as one argument; empty if missing) |
| `$ARGUMENTS` or `$@` | Raw arguments string as typed |
| `${1:-fallback}` | `$1` if provided and non-empty, otherwise `fallback` |
| `${@:-fallback}` | All arguments if any were given, otherwise `fallback` |
| `${@:2}` / `${@:2:3}` | Slice of arguments starting at `$2` |
