# TUI Keybindings

This is the canonical reference for keyboard shortcuts, slash commands, and the
which-key panel in the TypeScript TUI (`packages/hya-tui-ts`). Defaults come from
[`packages/hya-tui-ts/src/upstream/config/keybind.ts`](../packages/hya-tui-ts/src/upstream/config/keybind.ts).
Override any binding through TUI config `keybinds` (see
[Configuration](configuration.md)).

For screens, dialogs, transcript, and prompt behavior, see
[TUI Reference](tui-reference.md).

## Leader-key model

| Setting | Default | Meaning |
| --- | --- | --- |
| `leader` | `ctrl+x` | Arms a timed chord. Chord keys are written as `<leader>…` in the tables below. |
| `leader_timeout` | `2000` (ms) | How long the chord stays armed after the leader key. |

While a sequence is pending:

- **Escape** clears the pending sequence.
- **Backspace** pops one token from the pending sequence.

The leader key itself is not a command; it only prefixes multi-key chords such as
`<leader>n` (new session).

## How to read the tables

| Column | Meaning |
| --- | --- |
| **Command** | Internal command name (palette / binding map). |
| **Default binding** | Factory default. `unbound` means the default is `none` (no keys until you bind one). Multiple alternatives are comma-separated. |
| **Slash name** | Built-in slash command, if any. Parentheses list aliases. |
| **Meaning** | What the command does. |

`app.exit` fires only when the prompt is unfocused **or** the prompt input is
empty. `terminal.suspend` is disabled on Windows (`win32`); on those platforms
`ctrl+z` is reassigned to input undo when suspend is unavailable.
`prompt.paste` uses `preventDefault: false` so the terminal’s native paste can
still run alongside the handler.

## Binding collisions

Two defaults share the same chord:

| Chord | Commands |
| --- | --- |
| `<leader>q` | `session.queued_prompts` and the `<leader>q` alternative of `app.exit` |
| `<leader>h` | `session.toggle.conceal` and `tips.toggle` |

Which action wins depends on which binding layer is active. Prefer rebinding one
of the pair if you hit conflicts.

---

## App

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `app.exit` | `ctrl+c`, `ctrl+d`, `<leader>q` | `/exit` (`/quit`, `/q`) | Exit the application (only when prompt unfocused or empty). |
| `command.palette.show` | `ctrl+p` | — | Open the Commands palette. |
| `help.show` | unbound | `/help` | Open the help dialog. |
| `hya.status` | `<leader>s` | `/status` | Open the status dialog. |
| `app.debug` | unbound | — | Toggle the debug overlay. |
| `app.heap_snapshot` | unbound | — | Write a heap snapshot. |
| `app.toggle.animations` | unbound | — | Toggle UI animations. |
| `app.toggle.file_context` | unbound | — | Toggle file context. |
| `app.toggle.diffwrap` | unbound | — | Toggle word wrap in inline diffs (`word` ↔ `none`). |
| `app.toggle.paste_summary` | unbound | — | Toggle summarized large paste placeholders. |
| `app.toggle.session_directory_filter` | unbound | — | Toggle session list directory filtering. |

## Theme

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `theme.switch` | `<leader>t` | `/themes` | Open the theme picker (live-previews; reverts if dismissed). |
| `theme.switch_mode` | unbound | — | Switch between light and dark mode. |
| `theme.mode.lock` | unbound | — | Lock or unlock the current light/dark mode. |

## Session

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `session.sidebar.toggle` | `<leader>b` | — | Toggle the session sidebar (auto ↔ hide). |
| `session.toggle.scrollbar` | unbound | — | Show or hide the transcript scrollbar. |
| `session.export` | `<leader>x` | `/export` | Export the session transcript (filename and options dialog). |
| `session.copy` | unbound | `/copy` | Copy the full session transcript to the clipboard. |
| `session.new` | `<leader>n` | `/new` (`/clear`) | Navigate to Home (new session). |
| `session.list` | `<leader>l` | `/sessions` (`/resume`, `/continue`) | Open the Sessions dialog. |
| `session.timeline` | `<leader>g` | `/timeline` | Jump to a user message in the transcript. |
| `session.fork` | unbound | `/fork` | Fork the session from a chosen message (or full session). |
| `session.rename` | `ctrl+r` | `/rename` | Rename the current session. |
| `session.delete` | `ctrl+d` | — | Delete a session (in the Sessions dialog: press again to confirm). |
| `session.interrupt` | `escape` | — | Interrupt a non-idle turn (double Escape; see [TUI Reference](tui-reference.md#prompt-input)). |
| `session.background` | `ctrl+b` | — | Background synchronous subagents (when the backend advertises the capability). |
| `session.compact` | `<leader>c` | `/compact` (`/summarize`) | Summarize / compact the session. |
| `session.toggle.timestamps` | unbound | `/timestamps` (`/toggle-timestamps`) | Show or hide message timestamps. |
| `session.toggle.generic_tool_output` | unbound | — | Expand or collapse generic tool output. |
| `session.queued_prompts` | `<leader>q` | — | Manage queued prompts. |
| `session.pin.toggle` | `ctrl+f` | — | Pin or unpin a session in the Sessions dialog. |
| `session.quick_switch.1` … `session.quick_switch.9` | `<leader>1` … `<leader>9` | — | Switch to the session in quick slot 1–9 (global layer). |
| `session.undo` | `<leader>u` | `/undo` | Undo (revert) the previous user message. |
| `session.redo` | `<leader>r` | `/redo` | Redo after a revert. |
| `session.toggle.conceal` | `<leader>h` | — | Toggle code-block concealment in messages. |
| `session.toggle.actions` | unbound | — | Show or hide completed tool details (`task` parts always stay visible). |
| `session.toggle.thinking` | unbound | `/thinking` (`/toggle-thinking`) | Expand or collapse thinking blocks. |
| `messages.copy` | `<leader>y` | — | Copy the last assistant message text. |

## Panes (subagents)

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `pane.roster` | `<leader>o` | — | Open the subagent roster (tab placement). |
| `pane.open.tab` | `<leader>T` | — | Open a subagent in a workspace tab. |
| `pane.open.vertical` | `<leader>V` | — | Open a subagent in a vertical split beside Main. |
| `pane.open.horizontal` | `<leader>S` | — | Open a subagent in a horizontal (stacked) split. |
| `pane.close` | `<leader>w` | — | Close the focused observation pane. |
| `pane.cycle` | `<leader>.`, `<leader>right` | — | Cycle pane focus forward. |
| `pane.cycle.reverse` | `<leader>left` | — | Cycle pane focus backward. |
| `pane.focus.main` | `<leader>0` | — | Focus the Main pane. |

In a multi-pane session, while an **observation** pane is focused, unmodified
digits `1`–`9` jump to the corresponding pane-strip entry (`1` = Main). One
unmodified Escape returns to Main. See [TUI Reference](tui-reference.md#pane-navigation).

## Model / Agent / Variant / MCP

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `model.list` | `<leader>m` | `/models` (`/mo`) | Open the model picker. |
| `model.dialog.favorite` | `ctrl+f` | — | Toggle favorite on the selected model (in the model dialog). |
| `model.cycle_recent` | `f2` | — | Next recently used model. |
| `model.cycle_recent_reverse` | `shift+f2` | — | Previous recently used model. |
| `model.cycle_favorite` | unbound | — | Next favorite model (toasts if none). |
| `model.cycle_favorite_reverse` | unbound | — | Previous favorite model. |
| `mcp.list` | unbound | `/mcps` | Open the MCP servers dialog. |
| `agent.list` | `<leader>a` | `/agents` | Open the agent picker. |
| `agent.cycle` | `tab` | — | Next primary agent. |
| `agent.cycle.reverse` | `shift+tab` | — | Previous primary agent. |
| `variant.cycle` | `ctrl+t` | — | Cycle model variants. |
| `variant.list` | unbound | `/variants` | Open the variant picker (hidden when the model has no variants). |

## Prompt

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `prompt.editor` | `<leader>e` | `/editor` | Open `$VISUAL` or `$EDITOR` for the current prompt. |
| `prompt.submit` | unbound | — | Submit the prompt (hidden; input uses `input.submit`). |
| `prompt.clear` | `ctrl+c` | — | Clear the input field. |
| `prompt.paste` | `ctrl+v` | — | Paste text or attach a clipboard image (`preventDefault: false`). |
| `prompt.skills` | unbound | `/skills` | Open the skill selector. |
| `prompt.stash` | unbound | — | Stash the current prompt and clear the input. |
| `prompt.stash.pop` | unbound | — | Restore the newest stashed prompt. |
| `prompt.stash.list` | unbound | — | List stashed prompts. |
| `prompt.editor_context.clear` | unbound | — | Clear editor context attached to the prompt. |
| `prompt.history.previous` | `up` | — | Previous prompt history item (at buffer start). |
| `prompt.history.next` | `down` | — | Next prompt history item (at buffer end). |
| `stash.delete` | `ctrl+d` | — | Delete a stash entry (press again to confirm in the stash dialog). |

## Input editing

These bindings apply while a managed prompt textarea has focus.

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `input.submit` | `return` | — | Submit input. |
| `input.newline` | `shift+return`, `ctrl+return`, `alt+return`, `ctrl+j` | — | Insert a newline. |
| `input.move.left` | `left`, `ctrl+b` | — | Move cursor left. |
| `input.move.right` | `right`, `ctrl+f` | — | Move cursor right. |
| `input.move.up` | `up` | — | Move cursor up. |
| `input.move.down` | `down` | — | Move cursor down. |
| `input.select.left` | `shift+left` | — | Select left. |
| `input.select.right` | `shift+right` | — | Select right. |
| `input.select.up` | `shift+up` | — | Select up. |
| `input.select.down` | `shift+down` | — | Select down. |
| `input.line.home` | `ctrl+a` | — | Start of line. |
| `input.line.end` | `ctrl+e` | — | End of line. |
| `input.select.line.home` | `ctrl+shift+a` | — | Select to start of line. |
| `input.select.line.end` | `ctrl+shift+e` | — | Select to end of line. |
| `input.visual.line.home` | `alt+a` | — | Start of visual line. |
| `input.visual.line.end` | `alt+e` | — | End of visual line. |
| `input.select.visual.line.home` | `alt+shift+a` | — | Select to start of visual line. |
| `input.select.visual.line.end` | `alt+shift+e` | — | Select to end of visual line. |
| `input.buffer.home` | `home` | — | Start of buffer. |
| `input.buffer.end` | `end` | — | End of buffer. |
| `input.select.buffer.home` | `shift+home` | — | Select to start of buffer. |
| `input.select.buffer.end` | `shift+end` | — | Select to end of buffer. |
| `input.delete.line` | `ctrl+shift+d` | — | Delete line. |
| `input.delete.to.line.end` | `ctrl+k` | — | Delete to end of line. |
| `input.delete.to.line.start` | `ctrl+u` | — | Delete to start of line. |
| `input.backspace` | `backspace`, `shift+backspace` | — | Backspace. |
| `input.delete` | `ctrl+d`, `delete`, `shift+delete` | — | Delete character. |
| `input.undo` | `ctrl+-`, `super+z` | — | Undo in input. |
| `input.redo` | `ctrl+.`, `super+shift+z` | — | Redo in input. |
| `input.word.forward` | `alt+f`, `alt+right`, `ctrl+right` | — | Move word forward. |
| `input.word.backward` | `alt+b`, `alt+left`, `ctrl+left` | — | Move word backward. |
| `input.select.word.forward` | `alt+shift+f`, `alt+shift+right` | — | Select word forward. |
| `input.select.word.backward` | `alt+shift+b`, `alt+shift+left` | — | Select word backward. |
| `input.delete.word.forward` | `alt+d`, `alt+delete`, `ctrl+delete` | — | Delete word forward. |
| `input.delete.word.backward` | `ctrl+w`, `ctrl+backspace`, `alt+backspace` | — | Delete word backward. |
| `input.select.all` | `super+a` | — | Select all. |

## Message scrolling

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `session.page.up` | `pageup`, `ctrl+alt+b` | — | Scroll up by half a page (implementation uses half viewport height). |
| `session.page.down` | `pagedown`, `ctrl+alt+f` | — | Scroll down by half a page. |
| `session.line.up` | `ctrl+alt+y` | — | Scroll up one line. |
| `session.line.down` | `ctrl+alt+e` | — | Scroll down one line. |
| `session.half.page.up` | `ctrl+alt+u` | — | Scroll up half a page. |
| `session.half.page.down` | `ctrl+alt+d` | — | Scroll down half a page. |
| `session.first` | `ctrl+g`, `home` | — | Jump to the first message. |
| `session.last` | `ctrl+alt+g`, `end` | — | Jump to the last message. |
| `session.message.next` | unbound | — | Navigate to the next message. |
| `session.message.previous` | unbound | — | Navigate to the previous message. |
| `session.messages_last_user` | unbound | — | Navigate to the last user message. |

## Dialog navigation

Active while a select-style dialog is open.

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `dialog.select.prev` | `up`, `ctrl+p` | — | Previous dialog item. |
| `dialog.select.next` | `down`, `ctrl+n` | — | Next dialog item. |
| `dialog.select.page_up` | `pageup` | — | Page up in dialog. |
| `dialog.select.page_down` | `pagedown` | — | Page down in dialog. |
| `dialog.select.home` | `home` | — | First dialog item. |
| `dialog.select.end` | `end` | — | Last dialog item. |
| `dialog.select.submit` | `return` | — | Submit selected item. |
| `dialog.prompt.submit` | `return` | — | Submit a dialog text prompt. |
| `dialog.mcp.toggle` | `space` | — | Toggle an MCP server in the MCP dialog. |
| `dialog.move_session.new` | `ctrl+m` | — | New project copy (move-session dialog). |
| `dialog.move_session.delete` | `ctrl+d` | — | Delete project copy. |
| `dialog.move_session.refresh` | `ctrl+r` | — | Refresh project copies. |

## Autocomplete

Active while prompt autocomplete is open.

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `prompt.autocomplete.prev` | `up`, `ctrl+p` | — | Previous autocomplete item. |
| `prompt.autocomplete.next` | `down`, `ctrl+n` | — | Next autocomplete item. |
| `prompt.autocomplete.select` | `return` | — | Select the highlighted item. |
| `prompt.autocomplete.complete` | `tab` | — | Complete the highlighted item. |
| `prompt.autocomplete.hide` | `escape` | — | Hide autocomplete. |

## Diff viewer

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `diff.close` | `escape`, `q` | — | Close the diff viewer. |
| `diff.toggle` | `enter`, `space` | — | Toggle the selected item. |
| `diff.expand` | `right` | — | Expand the selected item. |
| `diff.expand_all` | `E` | — | Expand all folders. |
| `diff.collapse` | `left` | — | Collapse the selected item. |
| `diff.switch_focus` | `tab` | — | Switch focus between patches and file tree. |
| `diff.next_hunk` | `]` | — | Next hunk. |
| `diff.previous_hunk` | `[` | — | Previous hunk. |
| `diff.next_file` | `n` | — | Next file. |
| `diff.previous_file` | `p` | — | Previous file. |
| `diff.toggle_file_tree` | `b` | — | Show or hide the file tree. |
| `diff.single_patch` | `s` | — | Toggle single-patch view. |
| `diff.switch_source` | `d` | — | Switch source (working tree / last turn). |
| `diff.toggle_view` | `v` | — | Toggle split or unified view. |
| `diff.help` | `?` | — | Show the Diff shortcuts table. |

The diff viewer also binds plugin-local scroll keys (`j`/`down`, `k`/`up`,
`pagedown`/`ctrl+f`, `pageup`/`ctrl+b`) and `m` to mark a file reviewed. Open
the viewer with `/diff` or the palette entry **Open diff viewer**.

## Terminal and tips

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `terminal.suspend` | `ctrl+z` | — | Suspend the terminal (disabled on `win32`). |
| `terminal.title.toggle` | unbound | — | Enable or disable dynamic terminal window titles. |
| `tips.toggle` | `<leader>h` | — | Show or hide Home tips (`tips_hidden` in KV). |
| `permission.prompt.fullscreen` | `ctrl+f` | — | Toggle fullscreen for the permission prompt. |

## Which-key panel

| Command | Default binding | Slash name | Meaning |
| --- | --- | --- | --- |
| `which-key.toggle` | `ctrl+alt+k` | — | Pin or unpin the which-key panel. |
| `which-key.layout.toggle` | `ctrl+alt+shift+k` | — | Switch dock ↔ overlay layout. |
| `which-key.pending.toggle` | `ctrl+alt+shift+p` | — | Auto-show on pending sequences (overlay mode). |
| `which-key.group.previous` | `ctrl+alt+left`, `ctrl+alt+[` | — | Previous command category tab. |
| `which-key.group.next` | `ctrl+alt+right`, `ctrl+alt+]` | — | Next command category tab. |
| `which-key.scroll.up` | `ctrl+alt+up`, `ctrl+alt+p` | — | Scroll bindings up. |
| `which-key.scroll.down` | `ctrl+alt+down`, `ctrl+alt+n` | — | Scroll bindings down. |
| `which-key.page.up` | `ctrl+alt+pageup` | — | Page up in the panel. |
| `which-key.page.down` | `ctrl+alt+pagedown` | — | Page down in the panel. |
| `which-key.home` | `ctrl+alt+home` | — | Jump to the first binding. |
| `which-key.end` | `ctrl+alt+end` | — | Jump to the last binding. |

The which-key builtin lists active bindings grouped by command category, with
tabbed groups, scrolling, an optional automatic pending-sequence preview in
overlay mode, and a footer that shows its own toggle and layout-switch
shortcuts. Layout and pending-preview preferences persist in KV as
`which_key_layout` and `which_key_pending_preview`.

**Default off:** the shipped which-key plugin sets `enabled: false` and is
filtered out of the static builtin host (`createBuiltinPlugins().filter(p =>
p.enabled !== false)`). The keybindings above exist in the map, but the panel
does not load unless that plugin is re-enabled.

---

## Slash commands

Slash commands are derived automatically from every **reachable, non-hidden**
command in the `palette` namespace that declares a `slashName` (or `slash.name`).
Typing `/` at **column 0** of the prompt opens slash autocomplete. Matching runs
over both the command title/display and its description.

| Slash | Aliases | Effect |
| --- | --- | --- |
| `/sessions` | `/resume`, `/continue` | Open the Sessions dialog. |
| `/new` | `/clear` | New session (navigate to Home). |
| `/models` | `/mo` | Open the model picker. |
| `/agents` | — | Open the agent picker. |
| `/mcps` | — | Open the MCP dialog. |
| `/variants` | — | Open the variant picker. |
| `/status` | — | Open the status dialog. |
| `/themes` | — | Open the theme picker. |
| `/help` | — | Open help. |
| `/exit` | `/quit`, `/q` | Exit the app. |
| `/rename` | — | Rename the current session. |
| `/timeline` | — | Jump to a user message. |
| `/fork` | — | Fork from a timeline point. |
| `/compact` | `/summarize` | Compact / summarize the session. |
| `/undo` | — | Undo the previous user message. |
| `/redo` | — | Redo after a revert. |
| `/timestamps` | `/toggle-timestamps` | Toggle message timestamps. |
| `/thinking` | `/toggle-thinking` | Toggle thinking block expansion. |
| `/copy` | — | Copy the full session transcript. |
| `/export` | — | Export the session transcript. |
| `/editor` | — | Open the external editor for the prompt. |
| `/skills` | — | Search and insert a skill as `/<skill> `. |
| `/diff` | — | Open the full-screen diff viewer. |

Plugins and other palette commands may register additional slash names at
runtime. The command palette (`ctrl+p`) is the authoritative live discovery
surface for whatever is currently registered.

## Related

- [TUI Reference](tui-reference.md) — screens, dialogs, transcript, prompt, themes
- [TUI Architecture](architecture/tui.md) — process chain and package ownership
- [Configuration](configuration.md) — YAML config and TUI overrides
