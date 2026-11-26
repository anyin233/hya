# Projects, Permissions & Safety

Giving an AI agent access to your files and terminal is useful only if you trust it not to wander into the wrong folder or run destructive commands behind your back. Hya uses two layers to keep you in control: **Projects** (where Hya is allowed to work) and **Permissions** (what Hya is allowed to run).

---

## Projects: Defining your workspace

Whenever you run `hya` inside a folder like `~/code/my-app`, Hya automatically creates or opens a **Project** for that folder.

Why have Projects instead of just "the current directory"?
1. **Your chats stay organized by project**, even if you start `hya` from a subfolder like `~/code/my-app/src/components` (`hya --continue` still finds your project's latest chat).
2. **One Project can include multiple folders.** Working on a frontend in `~/code/web` and an API in `~/code/api` at the same time? Add both folders as roots of one Project so Hya can read, search, and edit across both without nagging you on every file!

### Managing Projects (`/project`)

Type **`/project`** inside Hya to open the full-screen Projects manager:
- **`Enter`** switches to the highlighted Project and opens its latest chat.
- **`n`** creates a new Project (give it a name, then add one or more folder paths with `Tab` completion).
- **`e`** adds, removes, or reorders the folders (*roots*) of a Project (the first folder in the list is the primary root).
- **`t`** opens a **Temporary Session** (`--temp`) in an isolated scratch folder when you want to ask a quick question unrelated to any project.

---

## Folder boundaries (`ExternalDirectory`)

Hya's built-in file tools (`read`, `edit`, `write`, `glob`, `grep`, `patch`) check every file path—including symlinks—against your active Project's folders:

- **Inside your Project's folders:** File reads work freely, and edits follow your normal permission mode.
- **Outside your Project's folders** (for example `~/.ssh` or `../other-repo`): Hya stops and pops up an **`ExternalDirectory` permission prompt** asking whether to allow access to that outside folder. If you choose **Always allow**, that grant is saved *only* for the current Project, not globally across every project on your machine.

> [!NOTE]
> **Terminal commands (`bash`) have their own check.** Because a shell command like `python build.py` can read or write anywhere your user account can, Hya checks `bash` commands through command permissions rather than static path parsing. Always glance at `bash` prompts before approving them!

---

## Permission modes (`/permissions`)

A session's **permission mode** controls whether Hya asks you before taking action. Type **`/permissions`** to pick a mode (or `/permissions <mode>` to switch directly):

| Mode | How it behaves |
| --- | --- |
| **`manual`** *(default)* | Safe interactive mode. File searches/reads inside your Project run right away, while edits and shell commands ask you first unless allowed by a rule. |
| **`yolo`** | **No prompts at all.** Every tool call and command runs immediately—even ones that a rule would normally block. Use this only inside throwaway containers, VMs, or repos you can `git reset`. |
| **Bundle modes** | Custom smart-approval modes installed via Bundles (for example, auto-approving safe test commands while asking you for everything else). |

The first time you switch to `yolo`, Hya asks you to press **Enter** once to confirm so you never turn it on by accident. Your chosen mode applies to the whole chat *and* any helper subagents it spawns, and Hya remembers your preferred mode for new chats.

---

## Answering prompts and managing Saved Rules (`/rules`)

When Hya asks permission in the chat window:
- **`1`** — **Allow once** (just this one call).
- **`2`** — **Always allow** (saves a rule so Hya won't ask again for that exact command or tool).
- **`3`** or **`Esc`** — **Deny** (Hya receives a polite denial and adjusts its plan).

Changed your mind about clicking **Always allow** earlier? Type **`/rules`** to open the **Saved Rules** screen, highlight any saved permission, and press **`d`** to delete it. It takes effect right away.

---

## Pre-configuring rules in `config.yaml`

If there are commands you *always* want allowed (like `git status` and `cargo check`) or *always* want blocked (like `rm -rf` or `git push --force`), add regex rules to `~/.config/hya/config.yaml`:

```yaml
permission:
  model: default   # default | allow | strict | danger
  rules:
    # Evaluated in order—under `default`, the last matching rule wins!
    - target: command
      selector: "^git (status|diff|log)\\b"
      permission: allow

    - target: command
      selector: "\\b(rm\\s+-rf|git\\s+push\\s+--force)\\b"
      permission: deny

    - target: tool
      selector: "^(edit|write|apply_patch)$"
      permission: ask
```

Each rule picks `target: tool | mcp | command`, a Rust regex `selector` (use `^` and `$` when you want an exact full match), and `permission: allow | ask | deny`. See **[Configuration Reference](../reference/configuration.md)** for full details.
