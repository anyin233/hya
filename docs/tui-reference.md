# TUI Reference

User-facing reference for the TypeScript terminal UI (`packages/hya-tui-ts`):
screens, subagent panes, permission and question prompts, transcript and tool
rendering, dialogs, the prompt, and system chrome.

For the full keybinding tables and slash-command list, see
[TUI Keybindings](tui-keybindings.md). For process ownership, see
[TUI Architecture](architecture/tui.md).

---

## Screens

### Home route

Centered layout with the hya logo art and the tagline:

```text
The 100 Agents Who Really ×∞ Want to Help You
```

**Prompt.** Max width is 75 columns by default, or at least 75 and 70% of the
terminal width when `prompt.max_width` is `auto`. Plugin slots:

| Slot | Role |
| --- | --- |
| `home_logo` | Logo (replaceable) |
| `home_prompt` | Prompt body (replaceable) |
| `home_prompt_right` | Content to the right of the prompt |
| `home_bottom` | Below the prompt (tips, which-key hint, …) |
| `home_footer` | Bottom bar |

**Placeholders** rotate through example prompts:

| Mode | Prefix | Examples |
| --- | --- | --- |
| Normal | `Ask anything...` | `"Fix a TODO in the codebase"`, `"What is the tech stack of this project?"`, `"Fix broken tests"` |
| Shell | `Run a command...` | `"ls -la"`, `"git status"`, `"pwd"` |

**`--prompt`.** Auto-submits exactly once after sync and the model store are
ready, and only while the prompt text still matches the argument.

**Footer** (`home_footer`): destination directory (home-abbreviated; `:branch`
suffix when the destination matches the project directory), a `⊙ N MCP`
indicator with a `/status` hint when any MCP servers are configured, and the app
version on the right.

**Tips.** A random tip is shown when tips are not hidden and the user is not both
brand-new (zero sessions) and already connected to a provider. The seven tips
cover `@` file attach, `!` shell mode, `/undo`+`/redo`, `/models`, `/sessions`,
`/compact`, and `/help`. Toggle with `tips.toggle` (`<leader>h`); persistence key
`tips_hidden`.

### Session route layout

Top to bottom when a root session is open:

1. Optional **pane strip** (only when more than one workspace leaf exists)
2. One or more **workspace tabs** (Main plus subagent observation tabs)
3. Sticky-bottom **message scrollbox** (transcript)
4. Inline **permission** and **question** prompts when pending
5. **Prompt** input
6. **Toast** area
7. Optional **right sidebar**

There is **no separate status line** on the session route. Agent, model, variant,
and usage live in the prompt footer meta line.

**Other route kinds:**

| Kind | Behavior |
| --- | --- |
| **Plugin** | Rendered from the plugin runtime’s route registry. An unknown id shows `PluginRouteMissing` with a back-home affordance. |
| **diff** | Full-screen absolute overlay (`zIndex` 2500) registered by the `diff-viewer` builtin. |

### Sidebar

- **Width:** 42 columns, scrollable.
- **Title:** session title; when `HYA_CHANNEL != "latest"`, also shows the raw session id.
- **Visibility:** auto-shown when terminal width is greater than 120; otherwise a right-aligned overlay with a dark scrim (`RGBA` alpha 70). Hidden entirely for **child** sessions (`parentID` set). Toggle auto/hidden with `session.sidebar.toggle` (`<leader>b`).
- **Sections** (builtin plugins, top to bottom by order):
  - **Context** — total tokens of the last output-producing assistant message, percent of the model context limit, USD spend.
  - **MCP** — collapsible when more than two servers (header click); status dots labelled Connected / failed error / Disabled / Needs auth / Needs client ID.
  - **LSP** — collapsible; connected/error dots; empty states `LSPs are disabled` or `LSPs will activate as files are read`.
  - **Todo** — collapsible; shown only while at least one todo is incomplete.
  - **Modified Files** — collapsible; left-truncated paths with `+additions` / `-deletions`.
  - **Footer** — session directory home-abbreviated with optional `:branch` suffix (dim parent path, bright basename), green dot, `hya <version>`.

There is **no roster section** in the sidebar; subagents use panes and the roster
dialog instead.

---

## Subagent surface

### Pane navigation

**Pane strip.** When more than one leaf exists, a clickable row of `N:label`
chips (`main` plus each observation’s roster handle / `subagent_type` / truncated
session id). The focused chip is inverted to the accent color.

**Observation pane.** Read-only sticky-bottom transcript. Header line joins:

```text
handle - agent_type - Working|Finished|Failed|Cancelled|Idle - task - placement - focused|open - read-only
```

A spinner shows while Working. When focused, a hint line prints:

```text
ctrl+x ←/→ panes · 1-9 · esc main · ctrl+x w close
```

**Navigation:**

| Input | Effect |
| --- | --- |
| Unmodified digits `1`–`9` | Focus the corresponding pane-strip entry (`1` = Main). Only while an **observation** is focused and multiple panes exist. |
| Unmodified Escape | Clear any pending leader sequence and return to Main (refocuses the prompt unless a modal is open). |
| Bare `return` / unmodified single-character keys | Swallowed while an observation is focused (read-only), except while a leader chord is armed. |
| Leader then `←` / `→` / `w` / `.` / `0` | Cycle panes, close focused pane, cycle, or focus Main (fallback if the normal chord did not fire). |

### Subagent roster dialog

Opened by `pane.roster` (`<leader>o`) with Tab placement, or with placement
preselected via `pane.open.tab` (`<leader>T`), `pane.open.vertical`
(`<leader>V`, side-by-side), or `pane.open.horizontal` (`<leader>S`, stacked).

Renders an indented tree titled `Subagent roster - Tab|Vertical|Horizontal`. The
depth-0 row `Main · agent · sessionID` is selectable to return focus to Main.
Other rows show `handle · agent_type · lifecycle · task`, a spinner while
working, and an `open` / `focused` footer marker.

| In-dialog key | Action |
| --- | --- |
| `v` | Open in vertical split |
| `s` | Open in horizontal split |
| `r` | Retry after a fetch error |

**Lifecycle** (`resolveLifecyclePresentation`), preferring member status over
roster status:

| Source status | Label |
| --- | --- |
| `spawning` / `running` / `busy` | Working (spinner) |
| `done` | Finished |
| `failed` | Failed |
| `cancelled` | Cancelled |
| otherwise | Idle |

### Task / subagent rows in the transcript

`TaskMemberRow` renders one line per delegated member as:

```text
<Agent> Task[ (background)] — <description>
↳ <detail>
```

The detail line may show retry attempt, current tool title, tool-call count,
`Working...`, a summary, or `N toolcalls · duration`, with ✓ / ✗ / │ icons.
Clicking a row opens that subagent in a tab.

Members present in the run tree but not yet attached to a `task` tool part are
synthesized as extra rows on the last assistant message.

Whenever a turn has task UI, a hint line prints the `pane.roster` shortcut plus
`subagent roster`, and — only when the backend advertises
`experimentalBackgroundSubagents` and a non-background task is running — the
`session.background` shortcut plus `background`.

`session.toggle.actions` hides completed tool details but **always keeps**
`task` parts visible.

---

## Prompts and interaction

### Permission prompt

Inline three-way prompt titled **Permission required** with **Allow once** /
**Allow always** / **Reject**.

| Key | Action |
| --- | --- |
| `←` / `h`, `→` / `l` | Move between options |
| Return | Select |
| Escape | Reject |
| `permission.prompt.fullscreen` (`ctrl+f`) | Toggle fullscreen |

Tool-specific titles cover **bash** (icon `#`, title = tool `description` or
`Shell command`, body `$ <command>`), Edit, Read, Glob, Grep, List, WebFetch,
WebSearch, Task, external-directory access, repeated failure, and a generic tool
call.

**Allow always** opens a Confirm/Cancel stage warning that the patterns will be
allowed until hya is restarted. **Reject** (for child sessions, or via the
reject path) can open a free-text **Reject permission** editor: Return confirms
with the message, Escape cancels back.

### Question prompt

Multi-question flow with a tab strip (`←`/`h`, `→`/`l`, Tab), an option list
(`↑`/`k`, `↓`/`j`, numeric `1`–`N`), and Escape to reject.

**When Return submits vs toggles:**

| Case | Return on an option |
| --- | --- |
| Single question, not multi-select (`multiple !== true`) | Selects the option and **submits** immediately (no Confirm tab) |
| Multi-select question (`multiple === true`) | **Toggles** the option (`[✓]` checkbox); does **not** submit |
| Multi-question (more than one question), single-select | Selects and advances to the next tab |

When there is more than one question **or** the current flow is multi-select,
the tab strip includes a trailing **Confirm** tab (`questions.length + 1`). Move
to Confirm with Tab / `→` / `l`, then press Return on Confirm to submit all
answers. Footer hints show `toggle` while multi-selecting and `submit` on the
Confirm tab.

Multi-select options use `[✓]` checkboxes with a `(select all that apply)`
hint. A **Type your own answer** free-text row is available when custom answers
are allowed.

### Prompts aggregate across the subagent tree

A **root** session collects pending permissions and questions from every session
id in its run tree (falling back to its direct children when the tree is
unavailable), so a subagent’s prompt surfaces in the Main pane. A **child**
session route shows no prompts of its own. This is how the single-control-channel
invariant holds in practice.

---

## Transcript and tool rendering

### Transcript

**User messages:** agent-colored left border, hover highlight, click opens
Message Actions, MIME badges for attachments (`txt` / `img` / `pdf` / `dir`), a
`QUEUED` badge for messages ahead of the pending assistant message, optional
timestamps (`session.toggle.timestamps`), and a centered **Compaction** divider
when applicable.

**Assistant messages:** on the last or a finished message, a footer prints:

```text
▣ <Mode> · <model>[ · <duration>][ · interrupted]
```

The glyph is tinted by the agent color and muted when aborted. Non-abort errors
render in an error-bordered block.

**Revert:** at the revert point the transcript shows `N message reverted`, the
`session.redo` shortcut or `/redo`, and a per-file `+additions`/`-deletions`
list. Clicking opens a **Confirm Redo** dialog that dispatches `session.redo`.

### Reasoning / thinking blocks

| State | Display |
| --- | --- |
| Streaming | Spinner: `Thinking` or `Thinking: <title>` |
| Finished | `Thought[: title][ · duration]` |

In **hide** mode the block collapses to a single clickable `+`/`-` line so layout
does not jump. `[REDACTED]` placeholders are stripped; the body renders as muted
markdown.

Mode is a two-state `show` / `hide` value persisted in KV as `thinking_mode`
(default `hide`), migrated from the legacy `thinking_visibility` boolean, and
cycled with `session.toggle.thinking` (`/thinking`). `reasoningSummary` splits
an OpenAI-style `**Title**` header from the body.

### Tool rendering

Dedicated renderers exist for: `bash` (Shell UI), `glob`, `read`, `grep`,
`webfetch`, `websearch`, `write`, `edit`, `task`, `apply_patch`, `todowrite`,
`question`, `skill`. Everything else falls back to **GenericTool** via
`toolDisplay()`.

**Shell block:**

```text
# <description>[ in <workdir>]
$ <command>
```

ANSI is stripped from output; output is collapsed to 10 lines with click-to-expand
and a running spinner while active.

**Generic tool:** when `session.toggle.generic_tool_output` is on, prints
`# <tool> [k=v,…]` with output collapsed to 3 lines; otherwise a single
`⚙ <tool> [k=v,…]` inline row.

**Inline row states:** pending summary, icon + summary after completion,
strike-through when denied or rejected, error coloring on failure, click to
expand raw error text.

**Diagnostics:** up to three severity-1 diagnostics render as
`Error [line:col] message` under the written/edited path (host-normalized paths).

**Todo** rows use the shared `TodoItem` component also used by the sidebar.

### Inline diff rendering

Syntax-highlighted with line numbers. Split view when width is greater than 120
columns, forced unified when `diff_style: stacked`. Wrapping is controlled by the
`diff_wrap_mode` KV flag (`word` | `none`, toggled by `app.toggle.diffwrap`).
Per-file titles include `# Deleted`, `# Created`, `# Moved a → b`, and
`← Patched`.

### Diff viewer

Opened by `/diff` or the palette, navigating to the `diff` route in `git` mode
with the current `sessionID` and a `returnRoute`. Full-screen absolute overlay
(`zIndex` 2500).

Shows working-tree or last-turn diffs with a file-tree panel, per-file patches,
split/unified views, and a reviewed-file marker. **Switch source** offers
Working tree and Last turn.

| Constraint | Value |
| --- | --- |
| Split view minimum width | 100 columns |
| File tree width | 32 columns |

Persisted KV keys: `diff_viewer_show_file_tree`, `diff_viewer_single_patch`,
`diff_viewer_view`.

Global bindings (see [TUI Keybindings](tui-keybindings.md#diff-viewer)):
`escape`/`q` close, `enter`/`space` toggle, arrows expand/collapse, `tab` switch
focus, `]`/`[` hunks, `n`/`p` files, `b` tree, `s` single patch, `d` source, `v`
view, `?` help. Plugin-local: `j`/`k` scroll, page keys, `m` mark reviewed.
`?` opens an xlarge **Diff shortcuts** table.

---

## Dialogs and state

### Command palette

The **Commands** dialog (`command.palette.show`, `ctrl+p`) lists every reachable
non-hidden `palette`-namespace command with title, description, category
grouping, and formatted key bindings as a per-row footer. It hides itself from
its own list. It is the authoritative live discovery surface; the keybinding doc
mirrors the static defaults.

### Dialog catalog

| Dialog | Opened by | Notes |
| --- | --- | --- |
| **Sessions** | `session.list`, `<leader>l`, `/sessions` | Debounced server search, limit 30 results, date/recency grouping, pin/unpin (`ctrl+f`), delete (`ctrl+d`, press again to confirm), rename; footer hint for quick slots when filled |
| **Model** | `model.list`, `<leader>m`, `/models` | Grouped by provider, favorites section, release-date-aware sort; Favorite on `ctrl+f` |
| **Agent** | `agent.list`, `<leader>a`, `/agents` | Selectable primary agents; subtitle is `native` or the agent description |
| **Variant** | `variant.list`, `/variants` | Includes a Default entry; palette entry hidden when the model has no variants; otherwise toasts that the model has no variants |
| **MCP** | `mcp.list`, `/mcps` | Per-server status subtitles; toggle with `dialog.mcp.toggle` (`space`) |
| **Theme** | `theme.switch`, `<leader>t`, `/themes` | Live-previews on move/filter; reverts if dismissed |
| **Status** | `hya.status`, `<leader>s`, `/status` | MCP servers (Connected / Connecting… / error / Disabled in configuration / Needs authentication / needs client registration), LSP servers with roots, enabled formatters, loaded plugins with versions; close with Escape or the `esc` label |
| **Help** | `help.show`, `/help` | Minimal panel pointing at the palette shortcut; Return, Escape, `esc`/`enter` label, or `ok` |
| **Skills** | `prompt.skills`, `/skills` | `Search skills...` filter; inserts `/<skill> ` into the prompt |
| **Stash** | `prompt.stash.list` | Saved prompts with preview title and relative timestamp; delete on `stash.delete` (press again to confirm) |
| **Rename Session** | `session.rename`, `ctrl+r`, `/rename` | Text prompt seeded with the current title |
| **Timeline** | `session.timeline`, `<leader>g`, `/timeline` | Picker of user messages; scrolls the transcript on move; can seed the prompt |
| **Fork session** | `session.fork`, `/fork` | Full session plus each user message as a fork point; scrolls on move |
| **Message Actions** | Click a user message | Revert, Copy, Fork |
| **Export options** | `session.export`, `<leader>x`, `/export` | Filename default `session-<id8>.md`; Space-toggled switches for thinking, tool details, assistant metadata, open-without-saving; Tab between fields |
| **File picker** | Autocomplete / DialogTag | File attach picker |
| **Confirm / Alert / Prompt** | Various | Reusable `DialogConfirm`, `DialogAlert` (retry-error text), `DialogPrompt` |

### Session quick slots

Up to nine pinned **root** sessions are persisted and exposed as ordered quick
slots via `session.quick_switch.1`…`9` (`<leader>1`…`<leader>9`), registered on
the always-available `app.global` layer so they work from any pane. Pin or unpin
from the Sessions dialog with `session.pin.toggle` (`ctrl+f`). Stale pins are
filtered on read.

### Model recents and favorites

Persisted to `<state>/model.json` (`recent`, `favorite`, and per-model
`variant`). Load is best-effort: a missing file, non-object JSON, or non-array
`recent`/`favorite` is ignored with **no toast** (errors are swallowed). The
**Model …/… is not valid** warning toast fires when you **select** or
**favorite** a model the provider catalog does not serve—not when loading the
file. Cycle recent with `f2` / `shift+f2`. Cycle favorite is unbound by default
and toasts **Add a favorite model to use this shortcut** when no favorites
exist.

---

## Prompt

### Prompt input

Multi-line syntax-highlighted textarea with a rotating placeholder, agent-tinted
left border, height capped at `prompt.max_height` (default one-third of terminal
height, minimum 6), bracketed-paste CRLF/CR normalization, and a double-deferred
submit so IME composition flushes before send.

**Footer meta line (left):** agent name (or `Shell`), `· <model> <provider>`,
bold variant badge, with fade-in animations.

**Footer meta line (right):** context/cost usage when available; otherwise the
`agents` and `commands` shortcut hints. In shell mode: `esc exit shell mode`.

**Status line (while non-idle):** agent-colored block spinner (or a static
`[⋯]` when animations are off), a retry message with a live
`[retrying in Xs attempt #N]` countdown that opens a Retry Error alert when
truncated and clicked, and `esc interrupt` / `esc again to interrupt`.

**Double-Escape interrupt:** `session.interrupt` increments a counter that
resets after 5 seconds and aborts when the counter reaches 2 (second Escape).
In shell mode the first Escape only exits shell mode and does not count toward
interrupt. Inert while autocomplete is open or the prompt is unfocused.

### Shell mode

Typing `!` when the visual cursor is at **offset 0** in normal mode (autocomplete
closed, prompt not disabled) switches to shell mode—even if the buffer already
has text. The agent label becomes `Shell`, placeholders switch to the shell set,
and an `esc exit shell mode` hint appears. Escape or Backspace at offset 0
returns to normal mode.

### Prompt autocomplete

| Trigger | Completes | Notes |
| --- | --- | --- |
| `@` | Files, agents, reference aliases, MCP resources | Fuzzy threshold `0.5`; directory drill-down via a trailing `/` |
| `/` at column 0 | Slash commands | Match on title/display and description |

Keys: Up/`ctrl+p` and Down/`ctrl+n` move, Return selects, Tab completes, Escape
hides. Prompt history is disabled while autocomplete is open.

### Attachments, paste, history, stash

- **`prompt.paste` (`ctrl+v`):** inserts clipboard text or attaches a clipboard image as a `clipboard` file part. Large pasted text collapses into a summarized virtual extmark that expands on copy-out or in the external editor, controlled by `app.toggle.paste_summary` / KV `paste_summary_enabled` (seeded from `experimental.disable_paste_summary`).
- **History:** Up at buffer start walks back; Down at buffer end walks forward; restores text, parts, and shell/normal mode.
- **Stash:** `prompt.stash` saves and clears; `prompt.stash.pop` restores the newest entry; `prompt.stash.list` opens the dialog. A non-empty prompt is auto-stashed across prompt remounts.

### Automatic agent switching

- Switching to a session adopts the agent and model of its last user message when that agent is primary, unless `--agent` was passed on the CLI.
- A completed `plan_enter` tool call switches the local agent to `plan`; `plan_exit` switches to `build`. Switches are deduped by part id.

### External editor

`prompt.editor` (`<leader>e`, `/editor`) suspends the TUI and opens `$VISUAL` or
`$EDITOR` in the project worktree/directory, then re-imports the edited text and
re-anchors file/agent extmarks, dropping parts whose virtual text was deleted. A
non-zero exit surfaces `Editor exited with code/signal …`.

---

## Chrome and system integration

### Copy and clipboard

- Mouse-up and right-click copy the terminal selection and toast `Copied to clipboard`.
- Ctrl+C over a selection copies instead of exiting.
- Escape clears the selection.
- The OpenTUI console binds Ctrl+Y to copy-selection.
- All of the above are disabled when `HYA_DISABLE_COPY_ON_SELECT` is set, and are **always** disabled on `win32`.

**Transport:** native platform tools plus an OSC-52 escape, wrapped in the
`\x1bPtmux;…\x1b\\` DCS passthrough when `TMUX` or `STY` is set. Reads support
macOS PNG via osascript, Windows/WSL PowerShell images, and Wayland/X11 images
or text.

### Terminal window title

| Context | Title |
| --- | --- |
| Home | `hya` |
| Titled session | `hya \| <title>` (truncated to 40 characters) |
| Plugin route | `hya \| <route id>` |

Toggled by `terminal.title.toggle` (KV `terminal_title_enabled`) and suppressed
by `HYA_DISABLE_TERMINAL_TITLE`.

### Toasts

Border-colored variants: `info`, `success`, `warning`, `error`. Default duration
5000 ms. `toast.error` falls back to `An unknown error has occurred`. The backend
can raise a toast over `tui.toast.show`.

### Attention (notifications and sounds)

The `internal:notifications` builtin reacts to:

| Event | Message | Sound slot |
| --- | --- | --- |
| `question.asked` | Question needs input | `question` |
| `permission.asked` | Permission needs input | `permission` |
| Session goes idle after busy | Session done | `done` (root) or `subagent_done` (child) |
| `session.error` | Session aborted / Model stopped responding / Session error | `error` |

Subagent sessions get sound but no desktop notification. The attention service
tracks renderer focus/blur and gates notifications (`when: blurred` default) and
sounds (`when: always` default). Sound resolution order: config override → active
sound pack → builtin pack. Titles normalize to the product name `hya`.

**Slots:** `default`, `question`, `permission`, `error`, `done`, `subagent_done`.
The builtin pack ships bip-bop-01, bip-bop-03, staplebops-06, nope-03, and yup-01
mapped to those slots.

Configure under TUI `attention` keys. **`enabled` defaults to false** — attention
is off until you turn it on.

### Themes

33 shipped theme names:

`aura`, `ayu`, `catppuccin`, `catppuccin-frappe`, `catppuccin-macchiato`,
`cobalt2`, `cursor`, `dracula`, `everforest`, `flexoki`, `github`, `gruvbox`,
`kanagawa`, `material`, `matrix`, `mercury`, `monokai`, `nightowl`, `nord`,
`one-dark`, `osaka-jade`, `hya`, `orng`, `lucent-orng`, `palenight`, `rosepine`,
`solarized`, `synthwave84`, `tokyonight`, `vesper`, `vercel`, `zenburn`,
`carbonfox`

plus a generated **`system`** theme derived from the terminal.

**Precedence** when resolving the active theme name:
defaults &lt; plugin installs &lt; custom files &lt; generated `system`.
A custom file whose basename matches a shipped theme (for example `hya.json`)
**replaces** that default entry.

**Custom theme files on disk** (no rebuild required):

| Location | Path |
| --- | --- |
| User config | `$XDG_CONFIG_HOME/hya/themes/*.json` (fallback `~/.config/hya/themes/`) |
| Project / ancestors | From `cwd` up to filesystem root: `<dir>/.hya/themes/*.json` |

The JSON basename (without `.json`) is the theme name and appears in `/themes`
and as a valid `theme` config value. Valid files must be a JSON object with a
nested `theme` object key; invalid files are dropped silently.

Discovery order is config dir first, then each `.hya` from cwd to root. Within
that scan, **later directories overwrite earlier names**, so a root-most
ancestor’s `.hya/themes` wins over the current project’s file of the same name
(opposite of nearest-wins).

Default theme is `hya`. `theme.switch` (`<leader>t`) live-previews and reverts
on dismiss. `theme.switch_mode` and `theme.mode.lock` flip or pin light/dark.

**Live reload:** send `SIGUSR2` to the TUI process (`kill -USR2 <pid>`) to
re-detect the terminal light/dark palette and re-scan custom theme files. The
handler runs on a short delay ladder (250 ms, then 1000 ms); the custom-file
rescan runs only on the **last** tick, so a brief wait after the signal is
expected.

### Session epilogue on exit

On leave of a session, the TUI writes to stdout: the quadrant-block Hya art, the
tagline, `Session <title>`, and:

```text
Continue  hya-ts -s <sessionID>
```

Copying that line is the fastest resume path. The public launcher also accepts
`-s` / `--session` (see [TUI Architecture](architecture/tui.md)).

---

## Related

- [TUI Keybindings](tui-keybindings.md) — leader key, full binding tables, slash commands, which-key
- [TUI Architecture](architecture/tui.md) — process chain and package ownership
- [Configuration](configuration.md) — YAML and TUI config keys
