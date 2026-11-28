# Slash Commands & Keybindings

Press **`?`** on an empty input box (or run **`/help`**) anytime inside Hya to open a live, searchable list of every active shortcut and slash command.

---

## Essential keyboard shortcuts

| Key | Context | Action |
| --- | --- | --- |
| `Enter` | Composer | Send prompt (or queue it if a turn is already running) |
| `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | Composer | Insert a newline without sending |
| `Up` / `Down` | Composer (first/last line) | Cycle through your previously sent prompts |
| `@<text>` then `Tab` / `Enter` | Composer | Search project files and insert a `@path` file reference |
| `!<command>` then `Enter` | Composer | Run a shell command directly in the session without calling the model |
| `/` | Empty composer | Open and focus the command bar |
| `Ctrl+X` then `/` | Composer with draft | Open the command bar while keeping your current message draft intact |
| `Ctrl+V` | Composer | Paste text or an image from the system clipboard |
| `?` | Empty composer | Open the searchable key & command help overlay |
| `1` / `2` / `3` | Permission prompt | **1** Allow once · **2** Always allow · **3** Deny (digits also pick options on `ask_user` prompts) |
| `Esc` | Anywhere | Close open menu/view → switch Vim insert to normal → deny open prompt → leave read-only subagent view → **cancel running turn / stop active subagents** → clear input |
| `PgUp` / `PgDn` | Conversation / Pane | Scroll one page up or down |
| `Home` / `End` | Empty composer | Jump to top of transcript / jump to bottom and follow live output |
| `Alt+Left/Right/Up/Down` | `workspace` preset | Move keyboard focus between tiled panes |
| Mouse drag | Transcript | Select text and copy it to the clipboard automatically on release (OSC 52) |
| `Ctrl+C` twice (within 2s) | Composer | Quit Hya and archive the current session (first press clears input) |
| `Ctrl+D` | Empty composer | Terminal only: quit Hya and leave the session running in the background |

---

## Slash commands

### Sessions & Projects

| Command | Description |
| --- | --- |
| `/new [agent] [model]` | Start a new session in the active Project (`/clear` is an alias). |
| `/new --temp [agent] [model]` | Start an isolated temporary scratch session outside any Project. |
| `/sessions` | Open the session tree picker (`Enter` open, `F2` rename, `Ctrl+D` delete, `Ctrl+A` toggle archived, `F3` all Projects). |
| `/open <number \| id>` | Switch directly to a session by sidebar number (`1`, `2.1`) or `hysec_...` ID. |
| `/resume [<id>]` | Unarchive and reopen a session (or pick from archived + active sessions). |
| `/rename <title>` | Set the title of the current session. |
| `/undo` / `/redo` | Revert the last prompt *and* its file changes on disk / restore a pending undo before the next prompt. |
| `/fork` | Copy the conversation into a new session at the latest message or before any chosen prompt. |
| `/compact` / `/summarize` | Shrink earlier conversation turns right now to free context window space. |
| `/project` (`/projects`) | Open the full-screen Projects manager (`Enter` switch, `n` new, `e` edit roots, `r` rename, `d` delete, `t` temp session). |
| `/exit` (`/quit`) | Quit Hya and archive the active session. |
| `/to-background` | Terminal only: quit Hya while leaving the session running unarchived on the daemon. |

### Models, Agents & Permissions

| Command | Description |
| --- | --- |
| `/key` | Open the **Provider View** (`a` add provider, `k` set key, `x` remove key, `r` refresh models, `t` test model, `m`/`e`/`d` add/edit/delete model overrides). |
| `/model [<provider/model>]` (`/models`) | Open the model picker or switch directly (remembers choice per agent unless pinned in config). |
| `/effort [<level>]` (`/think`) | Pick or set reasoning effort (`default`, `none`, `low`, `medium`, `high`, `xhigh`, `max`). |
| `/agent [<id>]` | Open the **Agents View** (`Enter` switch, `m` set default model, `t` set default effort) or switch agent directly. |
| `/permissions [<mode>]` | Switch session permission mode (`manual`, `yolo`, or `<bundle>/<mode>`). |
| `/rules` | Open **Saved Rules** to inspect or delete (`d`) "Always allow" grants. |
| `/interactions` | List waiting permission or question prompts across sessions. |
| `/pending` | Jump straight to the oldest waiting prompt in another session. |
| `/approve <id>` / `/deny <id>` / `/answer <id> <text>` | Command-bar fallbacks for answering interaction prompts by ID. |

### Subagents, Workflows, Skills & Extensions

| Command | Description |
| --- | --- |
| `/subagents` | Open the Subagents selector and live viewer panes (`Ctrl+A` toggle archived, `s` steer, `Esc` interrupt). Also supports `select`, `view`, `pin`, `follow`, `steer <id> <msg>`, `interrupt <id>`, `archive <id>`. |
| `/workflow ...` (`/workflows`) | `list`, `info <name>`, `select <name>` (or `use`), `run [<name>] [k=v ...]`, `state`. |
| `/skills [<filter>]` | Browse all discovered Skills (`u` inserts `/skill:<name> ` into the input). |
| `/skill:<name> [<request>]` | Invoke a specific Skill on demand for this turn. |
| `/mcp` | Open **MCP Servers** (`Enter` inspect tools, `c` connect, `x` disconnect, `a` OAuth login, `r` refresh). |
| `/bundles` | Open **Bundles** (`i` install `.hyabundle`, `x` uninstall, `e` enable/disable, `t` trust TUI extension, `r` refresh). |
| `/extensions ...` | Inspect or manage TUI extensions (`enable`, `disable`, `reload`, `trust`, `untrust`, `sandbox`). |
| `/init` / `/review` | Built-in prompt workflows: generate an `AGENTS.md` or run a code review on uncommitted changes. |

### UI, Layout, Git & Diagnostics

| Command | Description |
| --- | --- |
| `/diff` | Open the side-by-side per-file Git working tree diff (`n`/`p` next/prev file, `r` reload). |
| `/todos` | Show the current session's todo list in the main panel. |
| `/status` | Show backend URL, daemon PID/DB/uptime, WebUI URL, active Project, agent, model, effort, and mode. |
| `/copy` | Copy the last assistant reply to the clipboard. |
| `/paste` | Paste text or image from the system clipboard. |
| `/editor` | Edit the current prompt draft in `$VISUAL` or `$EDITOR`. |
| `/vim [on\|off]` | Toggle Vim modal editing in the composer. |
| `/theme` | Live-preview and save a color theme (`auto`, `hya`, `light`, `contrast`, `ember`). |
| `/notifications [on\|off]` | Toggle desktop notifications when background turns finish or ask permission. |
| `/thinking [on\|off]` | Expand or collapse all model reasoning (`Thinking`) blocks. |
| `/tools [on\|off]` | Expand or collapse all tool call cards. |
| `/config [reload\|edit\|init]` | Inspect active `tui.toml` settings, reload from disk, edit in `$EDITOR`, or write a starter file. |
| `/set preset <name>` / `/set <component> on\|off` | Temporarily switch layout preset or toggle `startup`, `statusline`, `todos`, `subagents`, or `sidebar` for this session. |
| `/sidebar [on\|off]` / `/projects-sidebar [on\|off]` | Show or hide the right or left sidebars. |
| `/layout ...` | Manage panes in the `workspace` preset (`split`, `focus`, `close`, `reset`, `show`). |
| `/reload` (`/reload-<target>`) | Hot-reload all resources (or just `-config`, `-keybindings`, `-theme`, `-bundles`, `-extensions`, `-mcp`, `-commands`, `-skills`, `-context`, `-layout`, `-tui`). |
| `/refresh` | Re-fetch sessions, messages, models, skills, and commands from the server. |
| `/reconnect` | Reconnect to (or restart) the local database daemon after `hya serve stop`. |
| `/connect-remote [<link>]` / `/disconnect-remote` | Switch this TUI to a remote backend over a `hya://` relay link, or return to the local daemon. |
| `/api [METHOD /v2/path [JSON]]` | Browse `/v2` endpoints or send a raw JSON API call directly against the connected server. |

---

## Customizing shortcuts (`/keybind`)

You can bind any key (like `F6`, `Alt+G`, or `Ctrl+R`) to any full slash command—saved in `~/.config/hya/tui.json`:

```text
/keybind                           # Open the filterable shortcut browser
/keybind set Alt+G /tools on       # Bind Alt+G in conversation focus
/keybind set F6 /layout focus left # Bind F6 across the whole workspace
/keybind show Alt+G                # Check what a key or action does
/keybind unset Ctrl+C              # Disable a key completely
/keybind reset Alt+G               # Restore a single key to its default
/keybind reset all                 # Restore all default shortcuts
```

---

## Custom Markdown command placeholders

When writing `.hya/commands/<name>.md` or `~/.config/hya/commands/<name>.md` templates, the body supports positional and fallback substitutions:

| Placeholder | Expands to |
| --- | --- |
| `$ARGUMENTS` or `$@` | Everything typed after the command name |
| `$1`, `$2`, … | Individual whitespace-split arguments (supports `"quoted strings"`) |
| `${1:-default}` | First argument, or `default` when omitted |
| `${@:-default}` | All arguments, or `default` when none were passed |
| `${@:2}` / `${@:2:3}` | Arguments starting at `$2` / up to 3 arguments starting at `$2` |
