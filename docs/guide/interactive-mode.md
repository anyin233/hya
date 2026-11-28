# Interactive Mode (TUI & WebUI)

When you run `hya` in a terminal, you get two interfaces connected to the same background server:

- **The Terminal UI (TUI)** right where you ran the command.
- **The Browser WebUI** at `http://127.0.0.1:3250` (change the port with `hya --port 8000`, or pick a free port with `hya --port 0`).

Both draw the exact same screen, share the same chats, and stay in sync live. Start typing in your terminal, switch to your browser, or close either window—nothing is lost.

---

## Starting and coming back to chats

```sh
hya                # Start in the current folder
hya --continue     # Reopen the most recent chat in this Project
hya --resume       # Pick a past chat (including archived ones) from a list
```

Inside Hya, you can manage chats at any time:

- **`/new`** starts a fresh chat in your current Project (`/clear` does the same).
- **`/new --temp`** starts a throwaway scratch chat in a temporary folder (`~/.cache/hya/scratch/...`) without touching any Project.
- **`/sessions`** opens a searchable list of chats. Press **`F2`** to rename a chat, **`Ctrl+D`** to delete one, **`Ctrl+A`** to show or hide archived chats, or **`F3`** to look across all Projects.
- **`/exit`** (or pressing **`Ctrl+C` twice**) closes Hya and archives the chat.
- **`Ctrl+D`** on an empty input box (or `/to-background` in a terminal) closes the window while leaving the chat active in the background. In a browser tab, just close the tab!

---

## Choosing a screen layout (`tui.toml`)

By default, Hya uses a clean single-column layout (`statusline`): your chat history on top, the message box below it, and a one-line status bar at the bottom showing your model, permission mode, Git branch, and how full the context window is.

Want sidebars for Projects, Sessions, and Todos on a wide monitor? Switch layouts for the current session:

```text
/set preset workspace
```

There are four built-in presets:

| Preset | Best for | What it shows |
| --- | --- | --- |
| `statusline` *(default)* | Everyday terminal & browser use | Chat, boxed input, and a compact status bar underneath |
| `minimal` | Small screens, zero clutter | Just chat and input (model & mode sit on the input border) |
| `dock` | Borderless terminal feel | Borderless `╰─` prompt with live todos and running helpers stacked right above it |
| `workspace` | Wide monitors (150+ columns) | Resizable multi-pane workspace with Projects on the left and Sessions/Todos/Context on the right (`Alt+Arrows` switches panes) |

To keep your favorite layout permanently, run `/config init` and set `preset = "workspace"` in `~/.config/hya/tui.toml` (or run `/config edit` to open it in your editor).

### Bare mode (`hya --bare`, zero TUI bundles)

When running against a backend with no TUI bundles loaded (for example `hya --bare` with zero built-in bundles), the TUI settles immediately without waiting for `hya/basic-tui-components` and renders the clean single-column chat layout (`conversation` + `activity` + `composer` + bottom status line). Even if `~/.config/hya/tui.toml` selects `preset = "workspace"`, unavailable bundled sidebar panes (`projects`, `sessions`, `todos`, `context`, `jobs`, and dynamic `extension` panes) are hidden cleanly with no `"Loading…"` or missing-pane placeholders while preserving your saved `tui.json` layout. All core management overlays (`/key`, `/mcp`, `/skills`, `/bundles`, `/rules`, `/diff`) remain fully operational.

### Modular bundle panes & commands

In full mode, domain panes and slash commands are contributed dynamically by built-in `@hya/tui-sdk` v2 bundles rather than hardcoded in the TUI host:

- **Workspace panels (`hya/basic-tui-components`):** Provides the `Projects`, `Sessions`, `Todos`, `Context`, and `Jobs` panes plus the `/project` manager overlay.
- **Subagents (`/subagents`, `hya/multi-agent`):** Opens the dynamic Subagents roster and live Subagent Viewer panes (`hya/multi-agent#subagents`, `hya/multi-agent#subagent-viewer`) and supports `/subagents steer`, `/subagents archive`, and `/subagents interrupt`.
- **Workflows (`/workflow`, `/workflows`, `hya/workflow`):** Opens the dynamic Workflows pane (`hya/workflow#workflows`) and runs `/workflow select`, `/workflow run`, and `/workflow status`.
- **Plan & Agent Channels (`hya/plan`, `hya/agent-channels`):** Add custom tool-call cards and live status badges (`PLAN (read-only)` and unread `Mail` counts).

> **Upgrading from Hya v1:** Saved `tui.json` layouts use schema `version: 4` exclusively and `tui.toml` uses `version = 2`. Run `hya-migration run` (see **[Migrating from Hya v1 to v2](migration-v1-to-v2.md)**) to upgrade legacy v1 `tui.json` or `tui.toml` files; unmigrated legacy layouts are ignored and fall back to the default layout.

---

## Writing messages, attaching files, and running commands

The input box at the bottom does more than plain text:

- **Mention files with `@`:** Type `@` and part of a path (like `@auth`). Use **Up/Down** and **Tab** to insert `@src/auth.ts` so Hya knows right where to look.
- **Paste images and big snippets:** Press `Ctrl+V` (or `/paste`) to paste a screenshot straight from your clipboard—it appears as `[image #1]`. Pasting more than 10 lines of text folds into a tidy paste chip so your input box doesn't blow up.
- **Drag and drop in the WebUI:** Drop files or paste screenshots straight onto the browser tab (up to 20 MiB per file). The WebUI uploads them locally and inserts them into your prompt.
- **Run quick shell commands with `!`:** Start a line with `!` (for example `!git diff --stat` or `!pytest`) to run a shell command directly in your session without calling the AI model.
- **Queue messages while Hya is busy:** You don't have to wait for Hya to finish thinking. Type your next instruction and hit **Enter**—it waits in line (or steers the current work) automatically.
- **Vim mode & external editor:** Prefer Vim keys in the input box? Run `/vim on`. Writing a long prompt? Run `/editor` to draft it in `$EDITOR` (`vim`, `code --wait`, `nano`), then save and close to bring the text back.

---

## Rewinding mistakes (`/undo`, `/redo`, `/fork`)

- **`/undo`** steps back one prompt: it removes your last message and Hya's reply, **restores every file Hya edited, created, or deleted** during that turn, and puts your prompt back into the input box so you can reword it. Run `/undo` again to step back further.
- **`/redo`** brings back what you just undid (available until you send a new message).
- **`/fork`** copies the chat into a brand-new session—either from the latest message or from right before an earlier prompt—so you can try a second idea without losing the first.

> **Tip:** Already typed half a draft in the input box and don't want to erase it just to run a `/command`? Press **`Ctrl+X` then `/`** to open the separate command bar while keeping your draft untouched.

---

## Built-in management screens

Instead of editing YAML files by hand, you can manage almost everything inside Hya with slash commands (press **Esc** to exit any screen):

| Command | Screen | What you can do there |
| --- | --- | --- |
| `/key` | **Providers & Keys** | Add providers (`a`), save/replace API keys (`k`), refresh model lists (`r`), test a model (`t`), or edit context limits (`e`) |
| `/agent` | **Agents** | Switch between `hya-main`, `hya-plan`, or bundle agents, and pin each agent's default model (`m`) and thinking effort (`t`) |
| `/project` | **Projects** | Switch Projects, create multi-folder Projects (`n`), edit folder roots (`e`), or open a temporary session (`t`) |
| `/diff` | **Git Diff** | Inspect uncommitted changes and new files one file at a time (`n` / `p` next/prev file, `r` refresh) |
| `/mcp` | **MCP Servers** | Check connected MCP servers, inspect their tools, connect/disconnect (`c` / `x`), or complete OAuth login (`a`) |
| `/bundles` | **Bundles** | Install (`.hyabundle`), enable/disable (`e`), trust (`t`), or refresh (`r`) installed and project bundles |
| `/skills` | **Skills** | Browse every discovered `SKILL.md` and press `u` to use it in the prompt |
| `/rules` | **Saved Rules** | Review and delete (`d`) "Always allow" permission rules you saved earlier |

---

## Themes, copy-paste, and notifications

- **Themes (`/theme`):** Preview and save built-in color themes (`hya`, `light`, `contrast`, `ember`), or leave it on `auto` to match your terminal background.
- **Copying text:** Drag your mouse over any text in the chat—on release, Hya copies it to your system clipboard automatically. Or run `/copy` to copy Hya's entire last reply.
- **Desktop notifications (`/notifications on|off`):** When Hya finishes a long turn or stops to ask your permission while your terminal or browser tab is in the background, you get a desktop alert.

For the full list of slash commands and custom keybindings (`/keybind`), check **[Slash Commands & Keybindings](../reference/commands-and-keybindings.md)**.
