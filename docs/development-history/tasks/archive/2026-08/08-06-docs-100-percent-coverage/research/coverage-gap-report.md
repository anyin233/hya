# Documentation coverage gap report

Generated 2026-08-06 by a 19-agent audit. The feature surface was derived from
source code first; documents were read only in the diff step, so what exists was
discovered independently of what is claimed.

## Coverage summary

### Prose documentation, by feature axis

| Area | Features found | Documented | Gaps | Coverage |
| --- | ---: | ---: | ---: | ---: |
| CLI | 136 | 73 | 63 | 54% |
| Configuration | 121 | 64 | 57 | 53% |
| Tools & permissions | 87 | 47 | 40 | 54% |
| Providers | 172 | 88 | 84 | 51% |
| Runtime & events | 306 | 181 | 125 | 59% |
| Extensibility | 179 | 67 | 112 | 37% |
| TUI | 234 | 10 | 224 | 4% |
| **Total** | **1235** | **530** | **705** | **43%** |

324 gaps are actionable entries (undocumented, thin, stale, or contradicted).
65 further entries are doc claims the code no longer supports.

### Rust API documentation

Measured with `RUSTFLAGS="-W missing_docs" cargo check --workspace`.

| Crate | Undocumented public items |
| --- | ---: |
| `hya-proto` | 423 |
| `hya-core` | 403 |
| `hya-tool` | 389 |
| `hya-plugin` | 266 |
| `hya-bundle` | 195 |
| `hya-store` | 189 |
| `hya-sdk` | 175 |
| `hya-app` | 108 |
| `hya-provider` | 103 |
| `hya-mcp` | 72 |
| `hya-updater` | 67 |
| `hya-ts` | 23 |
| `hya-server` | 16 |
| `hya-client` | 7 |
| `hya-plugin-compat` | 3 |
| `hya` | 1 |
| **Total** | **2440** |

By kind: 1118 struct fields, 470 enum variants, 343 methods, 202 structs,
100 associated functions, 79 enums, 44 functions, 37 modules, 21 constants,
17 traits, 2 type aliases, 2 associated constants.

Three crates have no crate-level `//!` at all: `hya`, `hya-mcp`, `hya-plugin-compat`.
`hya-e2e` carries `#![allow(missing_docs)]`, which suppresses the lint.

### TypeScript package

`packages/hya-tui-ts` has no README, no `scripts/README.md`, no `test/README.md`,
and 0 docblocks across the audited export surface.

---
## Gaps by target document

### `docs/tui-keybindings.md` *(new)*

- **Leader-key model** — `leader` defaults to `ctrl+x`, arms a timed chord for `leader_timeout` ms (default 2000); Escape clears a pending sequence; Backspace pops one token. Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:41`.
- **Complete keybinding tables, one per category** (App, Theme, Session, Panes, Model/Agent/Variant/MCP, Prompt, Input editing, Dialog, Autocomplete, Diff viewer, Terminal) with columns Command | Default binding | Slash name | Meaning, covering every command in `keybind.ts` including the unbound-by-default ones (marked "unbound"): app.exit `ctrl+c,ctrl+d,<leader>q` (fires only when the prompt is unfocused or empty), command.palette.show `ctrl+p`, help.show (unbound, `/help`), hya.status `<leader>s` (`/status`), app.debug, app.heap_snapshot, app.toggle.animations, app.toggle.file_context, app.toggle.diffwrap, app.toggle.paste_summary, app.toggle.session_directory_filter, theme.switch `<leader>t`, theme.switch_mode, theme.mode.lock, prompt.editor `<leader>e`, session.sidebar.toggle `<leader>b`, session.toggle.scrollbar, session.export `<leader>x`, session.copy, session.new `<leader>n`, session.list `<leader>l`, session.timeline `<leader>g`, session.fork, session.rename `ctrl+r`, session.delete `ctrl+d`, session.interrupt `escape`, session.background `ctrl+b`, session.compact `<leader>c`, session.toggle.timestamps, session.toggle.generic_tool_output, session.queued_prompts `<leader>q`, session.pin.toggle `ctrl+f`, session.quick_switch.1-9 `<leader>1`…`<leader>9`, stash.delete `ctrl+d`, model.dialog.favorite `ctrl+f`, model.list `<leader>m`, model.cycle_recent `f2` / reverse `shift+f2`, model.cycle_favorite (+reverse, unbound), mcp.list, agent.list `<leader>a`, agent.cycle `tab` / reverse `shift+tab`, variant.cycle `ctrl+t`, variant.list, the scrolling family (session.page.up `pageup,ctrl+alt+b`, page.down `pagedown,ctrl+alt+f`, line up/down `ctrl+alt+y`/`ctrl+alt+e`, half-page `ctrl+alt+u`/`ctrl+alt+d`, first `ctrl+g,home`, last `ctrl+alt+g,end`), messages.copy `<leader>y`, session.undo `<leader>u` / session.redo `<leader>r`, session.toggle.conceal `<leader>h`, session.toggle.actions, session.toggle.thinking, prompt.submit (unbound) / prompt.clear `ctrl+c` / prompt.paste `ctrl+v` (`preventDefault:false` so terminal paste still works), prompt.skills / prompt.stash / prompt.stash.pop / prompt.stash.list / prompt.editor_context.clear (all unbound), input.submit `return` / input.newline `shift+return,ctrl+return,alt+return,ctrl+j`, the full input cursor-movement, selection, deletion and undo/redo tables, prompt.history.previous/next `up`/`down`, the dialog.* navigation set, the prompt.autocomplete.* set, permission.prompt.fullscreen `ctrl+f`, terminal.suspend `ctrl+z` (disabled on win32), terminal.title.toggle, tips.toggle `<leader>h`, and the diff.* set. Call out the two collisions: `<leader>q` is both session.queued_prompts and the app.exit fallback; `<leader>h` is both session.toggle.conceal and tips.toggle. Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:41,48`.
- **Slash-command section** — every built-in slash name with aliases and effect: /sessions (/resume, /continue), /new (/clear), /models (/mo), /agents, /mcps, /variants, /status, /themes, /help, /exit (/quit, /q), /rename, /timeline, /fork, /compact (/summarize), /undo, /redo, /timestamps (/toggle-timestamps), /thinking (/toggle-thinking), /copy, /export, /editor, /skills, /diff. Explain that they are derived automatically from every reachable non-hidden `palette`-namespace command, that `/` opens the slash autocomplete only at column 0, and that matching runs over both command title and description. This is the canonical location; README.md:111 and docs/getting-started.md:171 must be repointed here. Source: `packages/hya-tui-ts/src/upstream/keymap.tsx:260`.
- **Which-key panel** — a dock or overlay panel listing active bindings grouped by command category, with tabbed groups, scrolling, an optional automatic pending-sequence preview, and a footer showing its own toggle and layout-switch shortcuts. Commands: which-key.toggle, .layout.toggle, .pending.toggle, .group.previous, .group.next, .scroll.up, .scroll.down, .page.up, .page.down, .home, .end. Layout and pending-preview state persist in KV as `which_key_layout` and `which_key_pending_preview`. Source: `packages/hya-tui-ts/src/upstream/feature-plugins/system/which-key.tsx:10`.

### `docs/tui-reference.md` *(new)*

**Screens**

- **Home route** — centered hya logo art; tagline `The 100 Agents Who Really ×∞ Want to Help You`; max-width prompt (75 cols, or 70% of width when `prompt.max_width: auto`); toast area; plugin slots home_logo, home_prompt, home_prompt_right, home_bottom, home_footer. Rotating placeholders — normal: `Ask anything... "Fix a TODO in the codebase" | "What is the tech stack of this project?" | "Fix broken tests"`; shell: `Run a command... "ls -la" | "git status" | "pwd"`. `--prompt` auto-submits exactly once, only after sync and the model store are ready and only while the prompt text still matches the argument. Home footer: destination directory (home-abbreviated, `:branch` suffix when it matches the project dir), a `⊙ N MCP` indicator with a `/status` hint, app version on the right. Home tips: a random tip unless the user is brand new or no provider is connected; the seven tips cover `@` file attach, `!` shell, `/undo`+`/redo`, `/models`, `/sessions`, `/compact`, `/help`, with a `Configure a model to start coding` fallback; toggled by `tips.toggle` (`<leader>h`), persisted as `tips_hidden`. Source: `packages/hya-tui-ts/src/upstream/routes/home.tsx:23`.
- **Session route layout** — top to bottom: optional pane strip (only when more than one leaf exists), one or more workspace tabs (Main plus subagent observation tabs), a sticky-bottom message scrollbox, inline permission and question prompts, the prompt input, a toast area, and a right sidebar. **There is no separate status line** — agent, model, variant and usage live in the prompt footer meta line. Also list the third route kind, Plugin (rendered from the plugin runtime's route registry; an unknown id renders `PluginRouteMissing` with a back-home affordance), and the full-screen `diff` route (zIndex 2500) registered by the diff-viewer builtin. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:203`.
- **Sidebar** — 42 columns wide, scrollable, shows the session title (plus the raw session id when `HYA_CHANNEL != "latest"`); auto-shown when terminal width > 120, otherwise a right-aligned overlay with a 70-alpha scrim; hidden entirely for child sessions; toggled auto/hidden with `session.sidebar.toggle` (`<leader>b`). Sections: **Context** (total tokens of the last output-producing assistant message, percent of the model context limit, USD spend); **MCP** (collapsible above 2 entries via header click; status dots labelled Connected / failed error / Disabled / Needs auth / Needs client ID); **LSP** (collapsible, connected/error dots, empty states `LSPs are disabled` / `LSPs will activate as files are read`); **Todo** (collapsible, shown only while at least one todo is incomplete); **Modified Files** (collapsible, left-truncated paths with +additions/-deletions); **footer** (session directory home-abbreviated with `:branch` suffix, dim parent + bright basename, green dot, `hya <version>`). There is **no roster section**. Source: `packages/hya-tui-ts/src/upstream/routes/session/sidebar.tsx:10`.

**Subagent surface**

- **Pane navigation UX** — pane strip: when more than one leaf exists, a clickable row of `N:label` chips (`main` plus each observation's roster handle / subagent_type / truncated session id), focused chip inverted to the accent color. Observation pane: read-only sticky-bottom transcript with a header line `handle - agent_type - Working/Finished/Failed/Cancelled/Idle - task - placement - focused|open - read-only`, a spinner while working, and a focused hint `ctrl+x ←/→ panes · 1-9 · esc main · ctrl+x w close`. Navigation: unmodified digits 1-9 focus the corresponding pane-strip entry (1 = Main) in a multi-pane session; one unmodified Escape clears any pending leader sequence and returns to Main (refocusing the prompt unless a modal is open). Input safety: while an observation is focused, bare `return` and unmodified single-character keys are swallowed, except while a leader chord is armed. Leader fallback chords Ctrl+X then ←/→/w/./0 re-implement cycle/close/focus-main from the pending sequence. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:1471`.
- **Subagent roster dialog** — opened by `pane.roster` (`<leader>o`) in Tab placement, or with placement preselected via `pane.open.tab` (`<leader>T`), `pane.open.vertical` (`<leader>V`, side-by-side beside Main), `pane.open.horizontal` (`<leader>S`, stacked beside Main). Renders an indented tree titled `Subagent roster - Tab|Vertical|Horizontal`; the depth-0 row `Main · agent · sessionID` is selectable to return focus to Main; other rows show handle · agent_type · lifecycle · task with a working spinner and an `open`/`focused` footer marker. In-dialog keys: `v` vertical, `s` horizontal, `r` retry after a fetch error. Lifecycle mapping (`resolveLifecyclePresentation`): spawning/running/busy → Working (spinner), done → Finished, failed → Failed, cancelled → Cancelled, otherwise Idle, preferring member status over roster status. Source: `packages/hya-tui-ts/src/upstream/routes/session/dialog-subagent.tsx:8`.
- **Task/subagent rows in the transcript** — `TaskMemberRow` renders one line per delegated member as `<Agent> Task[ (background)] — <description>` plus a `↳` detail line carrying retry attempt, current tool title, N toolcalls, `Working...`, summary, or `N toolcalls · duration`, with ✓/✗/│ icons; clicking a row opens that subagent in a tab. Members present in the run tree but not yet attached to a `task` tool part are synthesized as extra rows on the last assistant message. Whenever a turn has task UI, a hint line prints the `pane.roster` shortcut plus `subagent roster`, and — only when the backend advertises `experimentalBackgroundSubagents` and a non-background task is running — the `session.background` shortcut plus `background`. `session.toggle.actions` hides completed tool details but always keeps `task` parts. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:2710,1878`.

**Prompts and interaction**

- **Permission prompt** — inline three-way prompt titled `Permission required` with `Allow once` / `Allow always` / `Reject`; ←/h and →/l move, Return selects, Escape rejects, `permission.prompt.fullscreen` (Ctrl+F) toggles fullscreen. Tool-specific titles for Edit, Read, Glob, Grep, List, WebFetch, WebSearch, Task, external-directory access, repeated failure, and a generic tool call. `Allow always` opens a Confirm/Cancel stage warning `This will allow <permission> until hya is restarted.` Reject opens a free-text `Reject permission` editor where Return confirms with the message and Escape cancels back. Source: `packages/hya-tui-ts/src/upstream/routes/session/permission.tsx:404`.
- **Question prompt** — multi-question tab strip navigated with ←/h, →/l and Tab; option list with ↑/k, ↓/j and numeric 1-N shortcuts; Return selects/submits; Escape rejects; `[✓]` checkboxes with a `(select all that apply)` hint for multi-select; a `Type your own answer` free-text row. Source: `packages/hya-tui-ts/src/upstream/routes/session/question.tsx:22`.
- **Prompts aggregate across the whole subagent tree** — a root session collects pending permissions and questions from every session id in its run tree (falling back to its direct children when the tree is unavailable), so a subagent's prompt always surfaces in the Main pane; a child session route shows no prompts of its own. This is what makes ADR-0003's single-control-channel invariant true in practice. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:251`.

**Transcript and tool rendering**

- **Transcript** — user messages: agent-colored left border, hover highlight, click opens Message Actions, MIME badges for attachments (txt/img/pdf/dir), a `QUEUED` badge for messages ahead of the pending assistant message, optional timestamps (`session.toggle.timestamps`), and a centered `Compaction` divider. Assistant messages: on the last or a finished message a footer prints `▣ <Mode> · <model>[ · <duration>][ · interrupted]` with the glyph tinted by the agent color and muted when aborted; non-abort errors render in an error-bordered block. Revert: at the revert point the transcript shows `N message reverted`, the `session.redo` shortcut or `/redo`, and a per-file +additions/-deletions list; clicking opens a `Confirm Redo` dialog dispatching `session.redo`. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:1720,1544`.
- **Reasoning / thinking blocks** — while streaming a `Thinking[: title]` spinner shows; once finished it reads `Thought[: title][ · duration]`. In `hide` mode the block collapses to a single clickable `+`/`-` line so layout never shifts. `[REDACTED]` placeholders are stripped and the body renders as muted markdown. Two-state `show`/`hide` mode persisted in KV as `thinking_mode`, default `hide`, migrated from the legacy `thinking_visibility` boolean, cycled with `session.toggle.thinking`. `reasoningSummary` splits an OpenAI-style `**Title**` header from the body. Source: `packages/hya-tui-ts/src/upstream/context/thinking.ts:24`.
- **Tool rendering** — dedicated renderers for bash/Shell, glob, read, grep, webfetch, websearch, write, edit, task, apply_patch, todowrite, question, skill; everything else falls back to `GenericTool` via the `toolDisplay()` dispatch. Shell block: `# <description>[ in <workdir>]`, a `$ command` line, and stripped-ANSI output collapsed to 10 lines with click-to-expand and a running spinner. Generic tool output is hidden behind the `session.toggle.generic_tool_output` KV flag — enabled prints `# <tool> [k=v,…]` with output collapsed to 3 lines, otherwise a single `⚙ <tool> [k=v,…]` inline row. Inline row states: `~ pending…`, icon + summary after, strike-through when denied or rejected, error coloring on failure, click to expand raw error text. Diagnostics: up to three severity-1 diagnostics render as `Error [line:col] message` under the written/edited path, normalized for the host platform. Todo rows use the shared `TodoItem` component also used by the sidebar. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:2179`.
- **Inline diff rendering** — syntax-highlighted with line numbers; split view above 120 columns but forced unified when `diff_style: stacked`; wrapping controlled by the `diff_wrap_mode` KV flag (`word` | `none`, toggled by `app.toggle.diffwrap`); per-file titles `# Deleted`, `# Created`, `# Moved a → b`, `← Patched`. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:2847`.
- **Diff viewer** — opened by `diff.open` (`/diff`, palette), navigating to the `diff` route in `git` mode with the current sessionID and a `returnRoute`; full-screen absolute overlay (zIndex 2500) registered by the `diff-viewer` builtin; shows working-tree or last-turn diffs with a file-tree panel, per-file patches, split/unified views and a reviewed-file marker. `Switch source` dialog offers `Working tree` and `Last turn`. Layout constraints: split needs ≥100 columns, file tree is 32 columns. Persisted KV keys `diff_viewer_show_file_tree`, `diff_viewer_single_patch`, `diff_viewer_view`. Bindings table: global diff.close `escape,q`, diff.toggle `enter,space`, diff.expand `right`, diff.expand_all `E`, diff.collapse `left`, diff.switch_focus `tab`, diff.next_hunk `]`, diff.previous_hunk `[`, diff.next_file `n`, diff.previous_file `p`, diff.toggle_file_tree `b`, diff.single_patch `s`, diff.switch_source `d`, diff.toggle_view `v`, diff.help `?`; plugin-local j/down, k/up, pagedown/ctrl+f, pageup/ctrl+b, `m` (diff.mark_reviewed). `?` opens an xlarge `Diff shortcuts` table. Source: `packages/hya-tui-ts/src/upstream/feature-plugins/system/diff-viewer.tsx:38`.

**Dialogs and state**

- **Command palette dialog** — the `Commands` dialog bound to `command.palette.show` (`ctrl+p`): every reachable non-hidden `palette`-namespace command with title, description, category grouping, and formatted key bindings as a per-row footer; it hides itself from its own list; it is the authoritative discovery surface mirrored by docs/tui-keybindings.md. Source: `packages/hya-tui-ts/src/upstream/component/command-palette.tsx:78`.
- **All dialogs, one entry each** — Sessions (`session.list`, `<leader>l`; debounced server-side search limited to 30 results, date grouping, inline pin/unpin `ctrl+f`, delete `ctrl+d` press-again-to-confirm, rename; `switch <ctrl+x 1…9>` footer hint when quick slots are filled); Model (`model.list`, `<leader>m`; grouped by provider with a favorites section and release-date-aware sort, `Favorite` on `ctrl+f`); Agent (`agent.list`, `<leader>a`; `Select agent`, selectable primary agents with `native` or the description as subtitle); Variant (`variant.list`; `Select variant` with a `Default` entry, palette entry hidden when the model has no variants, invoking it toasts `The current model does not support any variants.`); MCP (`mcp.list`; per-server status subtitles and `toggle` on `dialog.mcp.toggle` = space); Theme (`theme.switch`, `<leader>t`; live-previews on move/filter and reverts if dismissed); Status (`hya.status`, `<leader>s`; MCP servers with status dots and labels Connected / Connecting… / error / Disabled in configuration / Needs authentication / needs client registration, LSP servers with roots, enabled formatters, loaded plugins with versions; closes on esc or by clicking the `esc` label); Help (`help.show`; minimal panel pointing at the palette shortcut, closable with Return, Escape, the `esc/enter` label or the `ok` button); Skills (`prompt.skills`; `Search skills...` filter inserting `/<skill> ` into the prompt); Stash (`prompt.stash.list`; saved prompts with preview title and relative timestamp, `delete` on `stash.delete` press-again-to-confirm); Rename Session (`session.rename`, `ctrl+r`; text prompt seeded with the current title); Timeline (`session.timeline`, `<leader>g`; picker of user messages that scrolls the transcript on move and can seed the prompt); Fork session (`session.fork`; `Full session` plus each user message as a fork point, scrolling the transcript on move); Message Actions (click a user message: Revert, Copy, Fork); Export options (`session.export`, `<leader>x`; filename prompt defaulting to `session-<id8>.md` plus Space-toggled switches for thinking, tool details, assistant metadata and open-without-saving, Tab between fields); Autocomplete/DialogTag file picker; and the reusable DialogConfirm, DialogAlert (retry-error text) and DialogPrompt. Source: `packages/hya-tui-ts/src/upstream/component/dialog-session-list.tsx:130` and siblings.
- **Session quick slots** — up to nine pinned root sessions persisted and exposed as ordered quick slots via `session.quick_switch.1…9` (`<leader>1`…`<leader>9`), registered on the always-available `app.global` layer so they work from any pane; pin/unpin from the Sessions dialog with `session.pin.toggle` (ctrl+f); stale pins are filtered on read. Source: `packages/hya-tui-ts/src/upstream/context/local.tsx:453`.
- **Model recents and favorites** — persisted to `<state>/model.json`; invalid entries toast a warning rather than failing; `model.cycle_recent` / reverse bound to `f2` / `shift+f2`; `model.cycle_favorite` / reverse unbound and toast `Add a favorite model to use this shortcut` when no favorites exist. Source: `packages/hya-tui-ts/src/upstream/context/local.tsx:145`.

**Prompt**

- **Prompt input** — multi-line syntax-highlighted textarea with a rotating placeholder, agent-tinted left border, height capped at `prompt.max_height` (default ⅓ of terminal height, min 6), bracketed-paste CRLF/CR normalization, and a double-deferred submit so IME composition flushes before send. Footer meta line: agent name (or `Shell`), `· <model> <provider>`, a bold variant badge, with fade-in animations; right side shows context/cost usage plus the `agents` and `commands` shortcuts. Status line: while non-idle an agent-colored block spinner (or a static `[⋯]` when `app.toggle.animations` is off), a retry message with a live `[retrying in Xs attempt #N]` countdown that opens a Retry Error alert when truncated and clicked, and an `esc interrupt` / `esc again to interrupt` affordance. Triple-Escape interrupt: `session.interrupt` increments a counter that resets after 5 s and aborts on the third press; in shell mode the first Escape only exits shell mode; inert while autocomplete is open or the prompt is unfocused. Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:1305,1378`.
- **Shell mode** — typing `!` at column 0 of an empty prompt in normal mode switches to shell mode: the agent label becomes `Shell`, placeholders switch to the shell set, and an `esc exit shell mode` hint appears; Escape or Backspace at offset 0 returns to normal mode; the first Escape in shell mode does not count toward the triple-Escape interrupt. Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:800`.
- **Prompt autocomplete** — `@` completes over files, agents, reference aliases and MCP resources with a fuzzy threshold of 0.5 and directory drill-down via a trailing `/`; `/` at column 0 completes slash commands matched on both title and description. Keys: Up/Ctrl+P and Down/Ctrl+N move, Return selects, Tab completes, Escape hides (`prompt.autocomplete.prev/.next/.select/.complete/.hide`). Prompt history is disabled while autocomplete is open. Source: `packages/hya-tui-ts/src/upstream/component/prompt/autocomplete.tsx:60`.
- **Attachments, paste, history, stash** — `prompt.paste` (ctrl+v) inserts clipboard text or attaches a clipboard image as a `clipboard` file part; large pasted text collapses into a summarized virtual extmark that expands on copy-out or in the external editor, controlled by `app.toggle.paste_summary` / the `paste_summary_enabled` KV flag (seeded from `experimental.disable_paste_summary`). History: Up at buffer start walks back and Down at buffer end walks forward, restoring text, parts and shell/normal mode; disabled while autocomplete is open. Stash: `prompt.stash` saves and clears, `prompt.stash.pop` restores the newest entry, `prompt.stash.list` opens the dialog, and a non-empty prompt is auto-stashed across prompt remounts. Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:356`.
- **Automatic agent switching** — switching to a session adopts the agent and model of its last user message when that agent is primary, unless `--agent` was passed; a completed `plan_enter` tool call switches the local agent to `plan` and `plan_exit` to `build`, deduped by part id. Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:300`.
- **External editor** — `prompt.editor` (`<leader>e`, `/editor`) suspends the TUI and opens `$VISUAL` or `$EDITOR` in the project worktree/directory, then re-imports the edited text and re-anchors file/agent extmarks, dropping parts whose virtual text was deleted; a non-zero exit surfaces `Editor exited with code/signal`. Source: `packages/hya-tui-ts/src/upstream/editor.ts:27`.

**Chrome and system integration**

- **Copy and clipboard** — mouse-up and right-click copy the terminal selection and toast `Copied to clipboard`; Ctrl+C over a selection copies instead of exiting; Escape clears the selection; the OpenTUI console binds Ctrl+Y to copy-selection; all of it disabled by `HYA_DISABLE_COPY_ON_SELECT` (implicitly always on for win32). Transport: native tools plus an OSC-52 escape wrapped in the `\x1bPtmux;…\x1b\\` DCS passthrough when `TMUX` or `STY` is set; reads support macOS PNG via osascript, Windows/WSL PowerShell images, Wayland/X11 text. The terminal-environment context reports `multiplexer: "tmux"` when `$TMUX` is set and `"screen"` when `$STY` is set, plus display server and platform. Source: `packages/hya-tui-ts/src/upstream/clipboard.ts:27,101`.
- **Terminal window title** — `hya` on home, `hya | <title>` (truncated to 40 chars) for a titled session, `hya | <route id>` for a plugin route; toggled by `terminal.title.toggle` (persisted as `terminal_title_enabled`) and suppressed by `HYA_DISABLE_TERMINAL_TITLE`. Source: `packages/hya-tui-ts/src/upstream/app.tsx:429`.
- **Toasts** — border-colored info / success / warning / error variants with a 5000 ms default duration; `toast.error` falls back to `An unknown error has occurred`; the backend can raise one over `tui.toast.show`. Source: `packages/hya-tui-ts/src/upstream/ui/toast.tsx:10`.
- **Attention (notifications and sounds)** — triggers from the `internal:notifications` builtin: `question.asked` → `Question needs input`; `permission.asked` → `Permission needs input`; a session going idle after being busy → `Session done` (or the `subagent_done` sound for child sessions); `session.error` → `Session aborted` / `Model stopped responding` / `Session error`. Subagent sessions get sound but no desktop notification. The attention service tracks renderer focus/blur and gates notifications (`when: blurred` default) and sounds (`when: always` default), resolving each sound from config override → active sound pack → builtin pack, and normalizing titles to the `hya` product name. Slots: `default`, `question`, `permission`, `error`, `done`, `subagent_done`; the builtin pack ships bip-bop-01, bip-bop-03, nope-03, staplebops-06, yup-01. Point at the `attention` config keys and remember `enabled` defaults to **false**. Source: `packages/hya-tui-ts/src/upstream/feature-plugins/system/notifications.ts:29`; `packages/hya-tui-ts/src/upstream/attention.ts:119`.
- **Themes** — 33 shipped names (aura, ayu, catppuccin + frappe/macchiato, cobalt2, cursor, dracula, everforest, flexoki, github, gruvbox, kanagawa, material, matrix, mercury, monokai, nightowl, nord, one-dark, osaka-jade, hya, orng, lucent-orng, palenight, rosepine, solarized, synthwave84, tokyonight, vesper, vercel, zenburn, carbonfox) plus a generated `system` theme derived from the terminal. Precedence: defaults < plugin installs < custom files < system. Default is `hya`; `theme.switch` (`<leader>t`) live-previews and reverts on dismiss; `theme.switch_mode` and `theme.mode.lock` flip or pin light/dark. Source: `packages/hya-tui-ts/src/upstream/theme/index.ts:130`.
- **Session epilogue on exit** — the TUI writes the quadrant-block Hya art, the tagline, `Session <title>`, and a `Continue  hya-ts -s <sessionID>` line to stdout; note that copying that line is the fastest resume path and cross-link the `-s/--session` flag. Source: `packages/hya-tui-ts/src/upstream/util/presentation.ts:21`.

### `docs/troubleshooting.md`

- **`## Diagnosing Slow Startup`** — `HYA_STARTUP_TRACE=1` (truthy values exactly `1` or `true`, case-insensitive) emits structured startup marks on stderr from the Rust launcher, the backend, and the TS TUI; sample `HYA_STARTUP_TRACE=1 hya . 2>trace.log`. Source: `crates/hya-ts/src/main.rs:267-276`; `crates/hya-backend/src/serve.rs:106`; `packages/hya-tui-ts/src/hya/startup-trace.ts:8`.
- **Startup mark vocabulary** — in emission order: `bun_entry`, `backend_spawn` (only when the launcher auto-spawns), `theme_resolved`, `shell_paint` (value `immediate` unless `HYA_SYNC_PLUGIN_START` is set), `plugin_host_done`, `sync_partial`, `sync_complete`; each mark carries wall and monotonic timestamps. Note `HYA_SHOW_TTFD` adds an on-screen first-paint overlay and `HYA_FAST_BOOT` skips the loading overlay when comparing runs. Source: `packages/hya-tui-ts/src/hya/startup-trace.ts:8`.
- **`## Copy/Paste Does Not Work`** — with `TMUX` or `STY` set, hya wraps OSC-52 in a tmux/screen passthrough; with `WAYLAND_DISPLAY` set it prefers `wl-copy` over X11 tools. Tell Wayland users to install wl-clipboard and nested-multiplexer users that a stale `TMUX` variable breaks the passthrough. Source: `packages/hya-tui-ts/src/upstream/clipboard.ts:27,101`.
- **`## Provider Call Fails with http: <status>`** — `ProviderError::Http("{status}: {first 500 chars of body}")`; the body is truncated to 500 characters; **no retry is attempted at any layer**, so a 429 or 5xx fails the turn immediately. Once the stream is open, any SSE frame whose JSON carries an `error` object aborts the stream with `Http(message)` before reaching the decoder. Cross-link from docs/architecture/providers.md. Source: `crates/hya-provider/src/http.rs:517`; `crates/hya-provider/src/http/stream.rs:23`.
- **`## Image or file attachment fails with "does not support media type"`** — tell the user to switch the session to a `kind: google` route, since v2 prompt file attachments are replayed to providers as canonical media parts. Source: `crates/hya-provider/src/openai.rs:86`.
- **Extend `The TUI Does Not Start`** — the normal startup overlay: after 500 ms of not-ready a bottom-centered spinner reads `Loading plugins...` then `Finishing startup...`, held for at least 3 s; `HYA_FAST_BOOT` suppresses it entirely (the right flag when measuring startup). The error boundary: an unhandled render error replaces the UI with a full-screen crash view with a clickable reset button themed for the detected light/dark mode; reset re-mounts the app without restarting the process, so the backend session survives. Also add a pointer to the install.sh behaviour section in docs/getting-started.md (line 13 currently says only "reinstall with ./install.sh"), and a note that `$VISUAL`/`$EDITOR` selects the editor if `/editor` fails to open. Source: `packages/hya-tui-ts/src/upstream/component/startup-loading.tsx:5`.
- **Fix the provider-kind list at line 27** — replace the four-kind subset with a link to the docs/configuration.md table (see § Stale).
- **Fix the `models` mapping wording at line 39** — `providers.<id>.models` is a sequence, not a mapping (see § Stale).

### `docs/self-update.md`

- **Metadata validation rules and gate chain** — validation: platform, key_id, min_updater_version non-empty; `not_after >= not_before` (inclusive Unix seconds); artifacts non-empty, each with a non-empty name and a digest of exactly 64 **lower**-hex characters. Ordered gate chain in `apply`: (1) protocol_version, (2) min_updater_version by dotted-numeric compare against the crate version, (3) sequence strictly greater than the accepted floor, (4) platform equality, (5) not_before/not_after window, (6) trust-root lookup by key_id and Ed25519 verification. Source: `crates/hya-updater/src/verify.rs:33,135`.
- **`trust_roots.json` file format** — `{"roots":[{"key_id":"...","verifying_key_hex":"..."}]}`; at least one root required; key_id non-empty; verifying_key_hex exactly 64 **lower**-hex characters (uppercase is rejected — a hand-editing trap). Source: `crates/hya-updater/src/trust.rs:11`.
- **Staging and smoke-test constraints** — `--smoke` path must be **relative** and must not contain `..`; it runs as a child process from inside the staged release directory, never loaded into the updater's address space; a non-zero exit is `SmokeFailed`. Staging creates `root/releases/<sequence>` and **errors if it already exists** (re-applying the same sequence fails rather than overwriting), re-verifies each artifact's size and SHA-256 before writing, fsyncs each file, chmods 0o755 on Unix, and confirms every declared artifact landed. Source: `crates/hya-updater/src/stage.rs:27`; `crates/hya-updater/src/smoke.rs:16`.
- **`### Recovery rules`** — (a) no journal, or last phase committed/aborted → keep the current selector; (b) last phase `prepare` and the selector still points at the previous generation → write an `aborted` record and keep the old generation; (c) last phase `prepare` and the selector already points at the candidate → finish activation by writing the accepted floor and a `committed` record. Source: `crates/hya-updater/src/journal.rs:126`.
- **`discard` guardrails** — refuses sequence 0, the currently selected sequence, any sequence at or below the accepted floor, and a sequence whose staged directory is absent; frame as the safety property that discard can only remove never-accepted bits. Source: `crates/hya-updater/src/pipeline.rs:108`.
- **Back-link to `docs/skills.md`** for the `secure-self-update` built-in fallback skill (line 93/95).

### `docs/getting-started.md`

- **`install.sh` options table** — `--prefix DIR` (installs into DIR/bin, default /usr/local); `--bin-dir DIR` (installs directly into DIR, overrides --prefix, relative paths resolved against the script directory); `--profile release|dev|debug` (selects the cargo profile and matching target dir honouring `CARGO_TARGET_DIR`, exits 2 on any other value); `--dry-run` (prints every action, skips building and installing, prints verification commands instead of running them); `-h`/`--help` (usage, exit 0). Source: `install.sh:48,53,58,62`.
- **`install.sh` runtime behaviour** — Bun preflight: `bun --version` must succeed or the install aborts; then `bun install --frozen-lockfile --production` in the staged runtime and a prune with `bun packages/hya-tui-ts/scripts/prune-sdk-server.ts`. Permission preflight: walks up to the nearest existing ancestor of the target bin and lib dirs and, if not a writable directory, prints the sudo and `--bin-dir "$HOME/.local/bin"` remedies and exits 1 — document this as the permission-denied fix. Atomic swap: stages into `.tmp.$$` paths, moves any existing install to `.bak.$$`, renames into place, with an ERR/INT/TERM trap calling `restore_install` so an interrupted install never leaves a half-installed hya. Post-install verification: runs the hya shim against a dead server with `--bun /bin/true`, then `hya --version`, `hya-backend --help`, `hya-ts --help`, asserts every runtime file plus node_modules exists, and **fails if `command -v hya` does not resolve to the install path** (usual cause: an older hya earlier on PATH). Source: `install.sh:127,160,189,232`.
- **Correct and expand the key-controls table** — Escape: "Dismiss a dialog, hide autocomplete, clear a pending leader sequence, exit shell mode, return an observation pane to Main, or interrupt the running turn — press three times within 5 s to abort." Ctrl-C: "Copy the selection if there is one, clear the prompt if it has text, otherwise exit." Ctrl-D: "Exit when the prompt is empty and unfocused; deletes forward inside the prompt and deletes the highlighted entry in the Sessions and Stash dialogs." Add rows for `<leader>l` sessions, `<leader>m` models, `<leader>a` agents, `<leader>o` subagent roster, `<leader>b` sidebar, then link docs/tui-keybindings.md for the full set. Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:48`.
- **Repoint the slash-command cross-link at line 171** — from docs/cli.md to docs/tui-keybindings.md (see § Stale).

### `docs/development.md`

- **`## Dev tasks (cargo xtask)`** — all four dispatch targets. Note the hand-rolled positional dispatcher (not clap): the first positional selects the task, every remaining argument is forwarded verbatim, and an unrecognized task prints `usage: cargo xtask <sync-compat|migrate|startup-bench|matrix-check>` and **exits 0**. `sync-compat` (cross-link the recipe in docs/configuration.md), `migrate` (alias dispatching to the same implementation), `startup-bench` (honours `HYA_BACKEND_BIN`), `matrix-check` (validates `crates/hya-e2e/matrix.toml`, cross-link docs/testing/agent-matrix.md). State xtask is dev-only and ships in no binary. Source: `crates/xtask/src/main.rs:12`.
- **`HYA_BACKEND_DIR`** — read only by the SDK native-bridge example, naming the package directory for that bridge; example-only, no effect on `hya`, `hya-backend`, or the TUI; explicitly do not add it to the user-facing configuration reference. Source: `crates/hya-sdk/examples/native_spike.rs:9`.
- **`## TypeScript frontend` section** — `bun run build` = `bun build src/main.tsx --outdir dist --target bun --packages external`, producing `dist/main.js` plus copied audio assets (and state whether it is part of the install/release path). `bun run typecheck` = `tsgo --noEmit` over `src` and `test` with jsx=preserve and jsxImportSource=@opentui/solid. `bun test` — name the suites: boundary, branding-pruning, sdk-spine, runtime-boundary, startup-trace, agent-visibility, task-presentation, subagent-workspace, pty-smoke, real-backend, real-backend-agents. State the hard prerequisite that both the runtime and the test runner preload `@opentui/solid/preload` via `bunfig.toml` — the first thing to check when a fresh checkout fails to render or a test fails to compile. `scripts/prune-sdk-server.ts` — post-install step that rewrites the installed `@opencode-ai/sdk` export map down to only the v2 client, deletes server/process bundles, and probes that `createOpencodeClient` still imports; re-run after any SDK dependency bump. `scripts/generate-logo-art.py` — regenerates `component/logo-art.data.ts` and `util/epilogue-art.data.ts` from the 8-bit Hya wordmark PNG; re-run only when the wordmark asset changes. Source: `packages/hya-tui-ts/package.json:11`.

### `docs/testing/process-e2e.md`

- **TUI automation hooks subsection** — `HYA_ROUTE` (a JSON value parsed at TUI startup that overrides the initial route — home / session+sessionID / plugin+id; malformed JSON throws during boot, so quote carefully) and `HYA_FAST_BOOT` (any truthy value skips the initial loading screen, enabling deterministic screen assertions). Mark both explicitly as test/automation-only hooks outside the supported user configuration surface. Source: `packages/hya-tui-ts/src/upstream/app.tsx:249,250`.

### `docs/project-structure.md`

- **Add missing crates to the Crate Responsibilities table** — `hya-sdk` (typed `Client` trait over an HTTP or in-process-stdio `Transport`, the `DIRECTORY_HEADER` wire constant `x-opencode-directory`, `ServerHandle` process supervision, the live `MessageStore`, the frontend `TeamProjection` mirror, and the `session.next.*` `V2Event` reducer); `hya-native` (in-process native bridge: builds `hya_server::router(AppState)` via `hya_app::HyaRuntime` and drives it with `tower::ServiceExt::oneshot` instead of HTTP, injecting the directory header on every request — the Rust analogue of the compat adapter's in-process `app.fetch`; plus `spawn_event_bridge`, which subscribes to the in-process `GET /global/event` SSE stream, decodes each frame into `hya_sdk::GlobalEvent`, forwards it to an mpsc sender, **tolerates undecodable frames** by skipping them, re-subscribes after a 50 ms backoff on stream loss, and stops when the receiver is dropped); `hya-updater` (independent self-update TCB, cross-linked to docs/self-update.md). Source: `crates/hya-sdk/src/lib.rs:6`; `crates/hya-native/src/transport.rs:20`; `crates/hya-native/src/events.rs:23`.
- **Add a `hya-sdk` module map** — client.rs, native.rs, server.rs, events.rs, store.rs, team.rs, reducer.rs, types.rs, pending.rs, error.rs — mirroring the existing hya-proto/hya-core module tables.
- **Replace the three-row `hya-store` file table with a full module map** — lib.rs (connections, append/replay/read_projection, list/delete sessions, token ledger, decode_session_key), admission.rs (durable spawn admission journal), mailbox.rs (event-sourced mail writes, resident recovery, stop/failure finalization), resident_claim.rs (actor claim fencing primitives), sync.rs (compat sync history/replay), permission.rs (saved permissions), bundle_registry.rs (separate registry DB), error.rs; plus all migrations 0001-0008 with one line each and `bundle_migrations/0001_init.sql` as a separate migration set for a separate database. Source: `crates/hya-store/src/lib.rs:41`.
- **Fix the `hya-core` module map** — delete the `team.rs` row (file and `TeamControlPlane` removed per ADR-0001) and add rows for mailbox.rs (mailbox service loop draining `MailboxRequest`), engine/mailbox.rs (team-root mail delivery, roster/channel queries, `MAIN_HANDLE`), resident.rs (`ResidentSupervisor`, `TeamState`, per-team lock and quiescence), orchestrator.rs (`SubagentLimits`, `SubagentGovernor`, stream permits, per-team budgets), runtime_registry.rs (`RuntimeRegistry`, `TurnBinding`, `ConfigGeneration` publication), sidecar.rs (`SidecarLifecycle` contract), prompt.rs, title.rs. Source: `crates/hya-core/src/resident.rs:1319`.
- **`hya-bundle` / `hya-store` API rows** — for hya-bundle name the catalog entrypoints `from_prepared` / `from_verified_catalogs` / `with_verified_catalogs` (immutable catalog indexing agents by stable id **and** by `bundle:<id>/agent/<local_id>`, resources by `(ExportKind, stable id)` plus bundle-local name and alias) and the public reads `resolve_agent`, `resolve_resource`, `bundle_resources`, `resolve_spawn`, `spawnable_agents`; for hya-store list the `BundleRegistryRecord` columns bundle_id, version, publisher, 32-byte source_digest, prepared_digest, prepared_bytes, installed_at, all under a monotonically increasing registry generation. Source: `crates/hya-bundle/src/catalog.rs:43`; `crates/hya-store/src/bundle_registry.rs:22`.
- **Correct the `hya-plugin-example` row** — it is a placeholder stub (`fn main() {}`) that does not speak the plugin protocol, reserved for a future deterministic native-plugin QA fixture (planned: a message.user.before marker, a chat.params temperature override, a tool.execute.before veto sentinel, and event logging to stderr); point readers at the docs/plugin-protocol.md worked example. Source: `crates/hya-plugin-example/src/main.rs:7`.
- **Correct the `xtask` row** — no longer a scaffold (see § Stale).
- **Correct the 16 KiB output-cap claim at line 132** — the global cap is 5000 characters (see § Stale).
- **Correct the builtin tool table at lines 112-129** — add `list_agents` and the five mailbox tools; fix the permission-action column, which currently mixes runtime planes ("Web planes", "Interaction plane", "Plan tool", "None") with `Action` values — the real actions are `webfetch`/`websearch` for the web tools and `Tool` for question/ask_user/plan_exit/invalid. Source: `crates/hya-tool/src/tool.rs:237-271`.
- **Correct the `SessionEngine` write-path claim at lines 181-182** and **the reducer idempotence claim at lines 69-71** (see § Stale).
- **Link the new `packages/hya-tui-ts/README.md`** from the component table at line 212.

### `docs/adr/0001-event-sourced-mailbox-and-channels.md`

- **Channel fan-out skips terminal residents** — add a Consequences bullet: when folding a channel-addressed `MailSent`, subscribers whose `RosterEntry` is resident **and** whose `RosterStatus` is Done or Failed are skipped, so a stopped actor's inbox stops growing. Correct the "every current subscriber" phrasing here and in CONTEXT.md:184-188 to "every current eligible subscriber" and define eligible. Source: `crates/hya-proto/src/projection.rs:630`.
- **`## Delivery rules`** — `append_direct_mail` runs BEGIN IMMEDIATE → optional claim fence → replay the root projection → **reject** with `MailboxRejected` for an unknown handle, a transient non-root target, or a stopped/terminal resident → append `MailSent` → commit; the caller publishes the returned Envelope only **after** commit. `append_channel_mail` uses the same writer-lock discipline but **counts** eligible subscribers instead of rejecting. Define `resident_member_is_eligible`: a non-resident member always counts; a resident counts only if its `RosterStatus` is neither Done nor Failed **and** it currently holds an `active` row in `resident_actor_claim`. Source: `crates/hya-store/src/mailbox.rs:38,500`.

### `docs/adr/0002-resident-actor-model-and-autonomous-main-agent.md`

- **Explicit-stop cursor advance** — an `AgentActivityChanged` with `status=Failed` **and** `current_task == "resident stopped"` is treated by the shared reducer as an explicit stop, not a failure: it jumps `resident_cursor` to the full inbox length so a later restart does not replay mail the stopped actor never needed to see. Name the exact literal — it is a load-bearing sentinel written by `finalize_resident_stop` and read by the reducer. Source: `crates/hya-proto/src/projection.rs:543`; `crates/hya-store/src/mailbox.rs:225`.
- **`resident_effect_terminal_events`** — enumerate exactly what recovery appends for a lost actor: `MemberFinished{status: Cancelled}` for every member still Spawning or Running; a `ToolError` carrying `value {"code":"STALE_ACTOR_CLAIM"}` for every tool part still Pending or Running; `MessageFinished{Cancelled}` for every unfinished assistant message. Clients can key off `STALE_ACTOR_CLAIM` to distinguish takeover cleanup from a genuine tool failure. Source: `crates/hya-store/src/mailbox.rs:342`.
- **`finalize_resident_stop` / `finalize_resident_failure`** — terminalize an actor with a reason (`resident stopped` for an explicit stop — the same literal the reducer keys on), abort its accepted/started admissions marking the started ones `logical_released`, append `AgentActivityChanged{Failed}`, and flip the claim to released; **idempotent** when a matching released claim already reached the same state. Returned shapes: `RecoveredResidentWork` / `RecoveredResidentOutcome = Idle | Queued{inbox_cursor} | AbortedRunning{inbox_cursor, queued_after}`, together with the envelopes to publish and the admissions aborted. Source: `crates/hya-store/src/mailbox.rs:225`.

### `docs/adr/0003-tmux-tui-single-input-readonly-panes.md`

- **Update the two stale consequences** — drop "No dedicated tab-next/tab-prev bindings are introduced" (the shipped keymap adds `<leader>left`/`<leader>right` cycling and unmodified digit 1-9 pane jumping) and drop the promised "new-output indicator" on manually-scrolled observation views (not implemented). Point at the new `## Subagent Workspace` section in docs/architecture/tui.md. Also update the "Subagent manager" naming to the shipped `Subagent roster - Tab|Vertical|Horizontal` and record the added `r` retry action. Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:571,1343`; `packages/hya-tui-ts/src/upstream/routes/session/dialog-subagent.tsx:8`.

### `docs/adr/0006-tui-session-reset-and-subagent-visibility.md`

- **Correct the `/new` reset claim** — `session.new` only calls `route.navigate({type:"home"})` and `dialog.clear()`; it issues no abort and touches no prompt bookkeeping, and the old session is left running untouched on the server. Source: `packages/hya-tui-ts/src/upstream/app.tsx:539-551`.
- **Correct the subagent-row and sidebar-roster claims** — `TaskMemberRow` renders a row per delegated member in **every** state, and tree-only members are synthesized as extra rows; the sidebar has **no** roster section (its sections are Context, MCP, LSP, Todo, Modified Files, footer). Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:2710,1878`; `packages/hya-tui-ts/src/upstream/feature-plugins/sidebar/*`.

### `crates/hya-sdk/src/reducer.rs` (module rustdoc)

- **Rewrite the module doc** — `apply()` is implemented, not a skeleton: `prompt.promoted` prepends a user entry; `step.started` prepends an assistant entry; text/reasoning started push parts and their deltas accumulate into them; `tool.input.started` creates a pending tool part and called/success/failed move it to running/completed/error; `prepend()` dedupes by message id. Keep and emphasise the one deliberate silence: `prompt.admitted` is a durable inbox row only and must never mutate the visible timeline. Source: `crates/hya-sdk/src/reducer.rs:33,264`.

### `packages/hya-tui-ts` (package docs and TSDoc)

- **`README.md` (new)** — see § New documents required.
- **`scripts/README.md` (new)** — see § New documents required.
- **`test/README.md` (new)** — see § New documents required.
- **`src/main.tsx`** — module docblock (package entrypoint: parses argv, chdirs to the project directory, builds `TuiInput`, hands off to upstream `run` under the `HyaPlatform` Effect service); `launch(argv, runner)` documenting every flag, the `--url` throw, positional-vs-`--project` precedence, the `process.chdir` side effect, and `runner` as a test seam; `runTui` explaining why `Effect.provideService(HyaPlatform, HyaPaths)` is required.
- **`src/hya/platform.ts`** — `HyaPaths` (XDG resolution order, every path is `<xdg>/hya`); `HyaPlatform` (what the Effect service is for and who provides it); `HyaFlag` (a line per flag — 4 of 6 currently have none; note `disableCopyOnSelect` is force-on for win32); `HyaVersion`/`HyaChannel` (injected by the launcher/build; `"local"` means an unpackaged dev run).
- **`src/hya/product.ts`** — module docblock (single source of truth for user-visible product identity; changes are branding changes covered by `test/branding-pruning.test.ts`) plus one-liners for the five constants and `terminalTitle()`.
- **`src/hya/audit.ts`** — module docblock explaining `auditSurface` is a machine-readable branding manifest consumed by the branding test, not runtime code.
- **`src/hya/static-host.ts`** — module + `createStaticPluginHost()` docblock: returns a `TuiPluginHost` starting only compiled-in builtins (no dynamic loading, per the excluded boundary); document parallel start, the reverse-order cleanup contract, the keymap Proxy auto-tracking `registerLayer` disposal, and why statuses are re-sorted into declaration order.
- **`src/hya/sdk-spine.tsx`** — `SdkSpineState` (what `sync` and `data` are); `observeSdkSpine(input, ready)` (headless provider-stack probe, 5 s timeout with the `"SDK spine timed out"` rejection, resolve-on-`ready` contract, asserted provider nesting order).
- **`src/hya/startup-trace.ts`** — add the missing docblock on the `StartupMark` union (what each phase means) and document the `HYA_STARTUP_TRACE` env var plus the `once` option.
- **`src/upstream/routes/session/subagent-workspace.ts`** — 28 exports, 4 docblocks: document the exported parse/tree types, `RunTreeParseError`, and each exported helper.
- **`src/upstream/routes/session/index.tsx`** — 14 exports, 1 docblock: at minimum `focusMainPromptOwnership` and the other exports consumed by `test/subagent-workspace.test.ts`.
- **`src/upstream/feature-plugins/system/diff-viewer-file-tree-utils.ts`** — 19 pure helpers, 0 docblocks.
- **`src/upstream/config/keybind.ts` and `src/upstream/keymap.tsx`** — 17 and 16 exports, 0 docblocks: document the `CommandMap` / `Definitions` shape, which `test/subagent-workspace.test.ts` asserts against.
- **`src/upstream/config/index.tsx`** — 20 exports, 0 docblocks: document `resolve()`, called directly from main.tsx with `{ terminalSuspend: process.platform !== "win32" }`, whose meaning is unexplained.

---

## New documents required

| Path | Justification | Outline |
| --- | --- | --- |
| `docs/plugin-protocol.md` | The plugin JSON-RPC ABI is the crate's whole external contract and exists nowhere in prose; 11 gaps land here. | Transport & frame classification · Method table (initialize, shutdown, event, tool/call, hook/`<name>`) · Error codes incl. VETO=1 · The eleven hooks (params, outcomes, posture) with the three dead hooks flagged · Hook posture model & precedence · initialize reply schema · Plugin tool declaration gotchas · Limits & timeouts · The two spawn modes · Supervision & restart budget · Event fan-out backpressure · Multiple plugins on one hook · Worked minimal plugin example |
| `docs/compat-plugins.md` | The bundled Bun compat adapter has 11 gaps and no home; today only a one-line mention in docs/configuration.md:630. | Adapter CLI · Method table · OpenCode→hya hook translation (lossy) · Dispose ordering · Discovery order & config directories · Environment variables · Plugin factory input object · Module shapes & path/npm resolution · Hook behavior quirks · Tool registry & result normalization · Event converter coverage · Pinned SDK versions |
| `docs/skills.md` | Skills are authorable by users and have zero authoring documentation; three axes independently proposed a Skills section, and a dedicated page prevents triplication. | SKILL.md format & required fence · Frontmatter field table (incl. the silent-skip trap) · Discovery search path & first-name-wins · The `skill` tool contract & 10-file cap · Built-in fallback skills & shadowing · Worked example |
| `docs/tui-keybindings.md` | The keymap and slash-command surface are the single largest documentation hole (234 features, 10 documented) and both README.md:111 and getting-started.md:171 promise a reference that does not exist. | Leader key & chord model · Keybinding tables per category (App, Theme, Session, Panes, Model/Agent/Variant/MCP, Prompt, Input, Dialog, Autocomplete, Diff, Terminal) · Documented collisions · Slash commands with aliases · Which-key panel |
| `docs/tui-reference.md` | The user-facing TUI behaviour (screens, dialogs, prompts, transcript, themes, attention) has no page at all; docs/architecture/tui.md is architecture-only. | Screens (Home, Session, Sidebar) · Subagent panes & roster · Permission & question prompts · Transcript & thinking blocks · Tool rendering & diffs · Diff viewer · Command palette & dialogs · Quick slots & model store · Prompt (input, shell mode, autocomplete, paste/history/stash, editor) · Copy & clipboard · Terminal title, toasts, attention · Themes · Session epilogue |
| `docs/architecture/admission-and-governor.md` | 1,487 lines of `admission.rs` and 736 lines of `admission.rs` in hya-core carry zero doc comments and zero prose; the state machine is safety-critical for spawn budgets. | Purpose & invariants · `SpawnAdmissionOutcome` & `begin_spawn_admission` ordering · Terminal reason strings · `finalize_spawn_admission` & exactly-once refund · Root-turn cleanup & recovered-actor abort · Store API (states, capacity caps, intent cap, input/record/outcome types, the 15 methods) · Cross-links to storage.md schema and runtime.md permits |
| `packages/hya-tui-ts/README.md` | The package has no README; a reader cannot learn that `src/main.tsx` requires `--url` and is unlaunchable without a backend. | What this is (frontend-only; link UPSTREAM.md + docs/architecture/tui.md) · Requirements (bun 1.3.14, running hya-backend) · Install · Commands table (build/test/typecheck) · Run + full flag list · Layout (`src/hya` vs `src/upstream`, theme assets, scripts, test) · Editing rules & upstream re-sync · Environment variables · Release-time scripts · bunfig.toml preload note |
| `packages/hya-tui-ts/scripts/README.md` | `prune-sdk-server.ts` is invoked by install.sh:205 and release.yml:116, asserted by two tests, and has zero comments and zero prose — the highest-risk undocumented artifact in the package. | `prune-sdk-server.ts` (argv[2] = runtime dir; rewrites the SDK export map so `./v2` → v2 client; deletes server/process dist files; verifies with a spawned import probe; callers; guarded by `test/runtime-boundary.test.ts`) · `generate-logo-art.py` (pointer to its own docstring + docs/research/terminal-icon-rendering.md) |
| `packages/hya-tui-ts/test/README.md` | Three of eleven suites are architecture guards whose failure messages confuse anyone who has not read them; nothing says which suites need a prebuilt backend or a PTY. | Track T scope · Which suites need `cargo build -p hya-backend --bin hya-backend` · Which spawn a PTY · Which are invariant guards (boundary, branding-pruning, runtime-boundary) and what they enforce |

Also required: add every new path above to the **Docs Map in `docs/README.md`**, and add `docs/skills.md` + `docs/tui-keybindings.md` to the "If you want to run hya" reading path.

---

## Stale or contradicted content

Grouped by file. Each item must be **corrected or deleted**, not merely supplemented.

### `README.md`

- **:111** — "docs/cli.md | `hya` commands, flags, and the TUI slash-command reference." docs/cli.md contains no slash command. Repoint at `docs/tui-keybindings.md`.
- **:79-80** — claims docs/configuration.md holds "the complete `HYA_*` environment-variable reference". The table lists 5; the code reads at least 21. Either scope the claim or land the missing rows first.
- **:74-77** — the block commented "# same commands work on hya-ts:" contains two `hya` invocations, duplicating lines 71-73 and demonstrating nothing. Change to `hya-ts oauth login …` / `hya-ts oauth status`. Source: `crates/hya-ts/src/lib.rs:106`.

### `docs/getting-started.md`

- **:171-172** — "For the full command and TUI slash-command reference, see the CLI Reference (cli.md)." Dead promise; repoint at `docs/tui-keybindings.md`.
- **:169-170** — same completeness claim about the HYA_* reference as README.md:79-80.
- **:54-55** — the Escape and Ctrl-C/Ctrl-D rows are wrong in the ways detailed under the target-doc list; rewrite both.

### `docs/cli.md`

- **:19** — "`--db <PATH>` | SQLite database path. Empty string uses an in-memory store." False for bare interactive startup, `sessions`, and `tail-session`, which remap an empty `--db` to `$XDG_STATE_HOME/hya/sessions.db`. Source: `crates/hya-backend/src/main.rs:45-67`.
- **:21** — the three-flag row "Accepted Compat-compatible global flags" hides that all three are no-ops and that `--log-level` is restricted to four literal values.

### `docs/configuration.md`

- **:412-414** — "hya reads the following `HYA_*` variables (verified against the source listed in each row)." The completeness claim is false: ~16 HYA_* variables are missing (all five HYA_SUBAGENT_*, HYA_EVENT_BUS_CAPACITY, HYA_DEFER_SIDEPLANES, HYA_STARTUP_TRACE, HYA_DB, HYA_BACKEND_BIN, HYA_TUI_TS_DIR, HYA_VERSION, HYA_CHANNEL, HYA_ROUTE, HYA_FAST_BOOT, plus the six TUI display flags), as are the five compat-adapter HYA_* variables. Scope the sentence to the table's actual coverage **and** land the rows.
- **:673-696 (Custom Commands)** — six documented discovery directories; only two exist (`{workdir}/.opencode/command`, `{workdir}/.opencode/commands`). `$HOME/.config/opencode/commands`, `$HOME/.config/opencode/command`, `$HOME/.config/hya/prompts`, `{workdir}/.hya/prompts` return zero grep hits across `crates/` and `packages/`. **Delete all four.** The "Project commands override user commands with the same file stem" sentence is meaningless with no user tier — delete it; files are collected and sorted by path. The frontmatter list also omits `subtask`. Source: `crates/hya-server/src/compat/command_sources.rs:8-14,45-72`.
- **:426-430** — the rows `COMPAT_WEBSEARCH_PROVIDER`, `PARALLEL_API_KEY`, `EXA_API_KEY` are sourced to `crates/hya-tool/src/websearch.rs`, which reads **no** environment variables at all; grep for all three across `crates/` and `packages/` returns zero hits. **Delete the three rows.** Source: `crates/hya-tool/src/websearch.rs:23-40`.
- **:136, :607** — `<workdir>/.hya/plugins/**/plugin.toml` implies recursive discovery; the scan is exactly one directory deep. Replace with `<workdir>/.hya/plugins/<name>/plugin.toml`. Source: `crates/hya-app/src/plugins.rs:8`.
- **:408** — "Omitting `permission` is equivalent to `model: default` with no rules; a permission-only config remains active…" Misleading in the starter-file case: `permission: {model: default, rules: []}` is treated as **absent** by `has_meaningful_permission`. Source: `crates/hya-app/src/config.rs:666-670`.
- **:627** — `timeout_ms` described as "Optional request timeout" with no default; state 30000 ms.
- **:628** — plugin `env` described as "Environment variables passed to the plugin process as configured" without stating that `{env:}`/`{file:}` templating does **not** apply here, unlike MCP `env` at :131-132.

### `docs/troubleshooting.md`

- **:27** — "`kind` is `openai`, `openai-compatible`, `anthropic`, or `google`" omits `openai-completion`, `openai-response`, `openai-codex`, `grok-build`, telling a user with a valid `kind: grok-build` route that their config is wrong. Replace with a link to the docs/configuration.md:182-191 table. Source: `crates/hya-app/src/config.rs:203-220`.
- **:39** — "make sure that exact model id appears as a key under a supported provider's `models` object." `models` is a **sequence**, not a mapping; entries are a bare string or a mapping with `id`. Source: `crates/hya-app/src/config.rs:178`.
- **:13** — "reinstall with ./install.sh" with no flag reference; add the pointer to the new install.sh options table.

### `docs/architecture/tools-and-permissions.md`

- **:38-43** — "Large string outputs are truncated at 16 KiB and include a truncation marker." The global cap is `MAX_TOOL_OUTPUT_CHARS = 5000` **characters**, keeping the **last** 5000 behind `[tool output truncated: original N chars; showing last 5000 chars]`; 16 KiB is only the shell tool's internal stdout/stderr cap. Source: `crates/hya-tool/src/output_cap.rs:11-29`; `crates/hya-core/src/engine/turn.rs:854`.
- **:22-23** — `write` documented as `{path, content}` and `edit` as `{path, old, new}`; the advertised schemas require `filePath`+`content` and `filePath`+`oldString`+`newString`. The short spellings are runtime-only aliases and fail provider-side schema validation. This directly contradicts docs/architecture/agent-tool-surface.md:158-166,274-278.
- **:16-36** — the builtin inventory table omits six registered canonical builtins: `list_agents`, `send`, `roster`, `channels`, `join`, `leave` (26 canonical names total).
- **:33** — the `skill` row says "skill path/name input | Skill content"; the tool accepts `{name}` only and returns a structured `<skill_content>` envelope with a 10-file sample.
- **:54** — `Resource` described as "Path, glob, command, subagent, or any resource"; the enum has nine shapes (adds Tool, Url, WebSearch, Skill).
- **:53** — `Action` given only as "such as `Read`, `Edit`, `Grep`, or `Bash`"; there are fourteen.

### `docs/architecture/agent-tool-surface.md`

- **:453-457** — the six-value `ToolError` category list is missing `overloaded`, `operation_id_conflict`, `operation_already_handled`, `unknown_agent_id`, `agent_spawn_not_allowed`, `unsupported_inline_agent_field`.
- **All source anchors** — every pinned line range in this file has drifted (`builtins()` 145-183 → 237, permission classes 127-141/265-272 → 184-215/539-547, `SEARCH_LIMIT` 76 → 130, hidden aliases 178-182 → 315-372, walker 289-300, GLOB 349-441 → 629-717, GREP 445-579 → 726-855, FIND 640-690 → 920-965). The prose remains accurate; the anchors make the doc unverifiable. Fix all of them.

### `docs/architecture/providers.md`

- **:12-16 (`Provider` row)** — "A route that can stream a `CompletionRequest` for supported models" no longer covers the trait, which has grown `configured_identity_v1` (fails closed) and `compact_responses`. An implementor reading only this table misses two methods with real defaults and real consequences.
- **:29-53 (`## HTTP Provider`)** — enumerates three families and three protocol sections while six `ProviderKind`s and five auth styles exist; the Responses wire (encoder, `output_index`-keyed decoder, typed-terminal requirement, compact endpoint) has no section at all, so the page reads as a complete protocol list while omitting half the shipped routes.
- **:46** — names the `anthropic-version` header without the pinned value `2023-06-01` or the fact that it is not configurable.
- **:65** — "finish reasons map to hya `FinishReason`" is vague where the Anthropic section gives an explicit mapping; supply the OpenAI table.
- **:93** — "image, video, and audio data are passed as validated base64 `inlineData`" understates a 13-entry MIME allowlist, 28 MiB/20 MiB caps, and `data:` URL header-vs-part MIME matching.
- **:105-106** — the FakeProvider bullet gives no `FakeStep` variants and no exhausted-turn contract.

### `docs/architecture/storage.md`

- **:110-111** — "live HTTP routes currently declare `usage_reporting: false`." **Inverted**: every HTTP route sets `usage_reporting: true`, and all four decoders extract real usage. Source: `crates/hya-provider/src/http.rs:172`.
- **:28-50 (Migrations)** — covers only 0001 and 0005; migrations 0002, 0003, 0004, 0006, 0007, 0008 and the separate `bundle_migrations/` set for a second SQLite database are absent. `admission_journal` is reduced to "gains nullable actor_id/actor_epoch columns", which understates a table with a composite PK, a 7-value state CHECK, five all-or-nothing binding columns, and two FIFO promotion index families.
- **:61-70** — documents what `append_event` inserts and says "SQLite assigns the monotonic seq" without stating the seq is a single **global** AUTOINCREMENT across all sessions, or that `event_log` has no FK to `session`.
- **:65-67** — documents the session-key **write** side only; the decode ordering rule is missing.

### `docs/architecture/runtime.md`

- **:22-23** — "All runtime events pass through `SessionEngine::emit`, which appends to the store and publishes the same envelope to the bus." False: `publish_live` publishes at seq 0 with **no** store write and is the path for every streaming delta; resident writes go through `emit_for_actor` → `commit_resident_mutation`.
- **:60-71** — the 8-step round list omits actor-claim validation, activation-hook health check, cancel-token check, compaction, the `chat_params` hook, permit acquisition, `StepStarted`/`StepFinished`, and — critically — that the permit is **dropped before tool execution**.
- **:193-202 (`## Compaction and Summaries`)** — describes `ModelSummarizer` + `compact_context` as the whole mechanism; the turn now first tries the provider's native `/responses/compact` and only falls back on `Ok(None)` or an error. The `HYA_COMPACTED_CONTEXT` and `<<<RESPONSES_COMPACT_ITEMS>>>` markers appear nowhere.
- **:205-209** — "Hookable surfaces include … permission asks …" There is **no** permission-ask method on `HookDispatcher`; permission callbacks live on `PermissionPlane` / the hya-plugin bridge. Correct the list to the six real trait methods.
- **:255-256, :264-266** — lists `team.rs` and states "`TeamControlPlane` models lifecycle transitions, mailbox messages, and task board state." The file does not exist and `TeamControlPlane` was deleted per ADR-0001.
- **:132** — "Prepared canonical hook IDs are limited to `event`, `tool.execute.before`, and `tool.execute.after`" is correct only for **bundles**; make the scoping explicit so the wider 11-name plugin vocabulary does not read as a contradiction.

### `docs/architecture/event-model.md`

- **:99-108** — presents idempotence as exactly `if env.seq.0 <= self.last_seq { return; }` and concludes duplicates can be safely ignored. A preceding `if env.seq.0 == 0` branch applies live-only envelopes unconditionally without advancing `last_seq`.
- **:29-45** — the "major event groups" list omits member lifecycle, team registration/roster, mail/channels, `CommandExecuted`, and `Event::Unknown`.
- **:67** — lists `Part::Media` in the Messages and Parts table directly above the Projection section without saying media has **no** `PartProjection` variant and does not survive the fold.

### `docs/project-structure.md`

- **:28** — "crates/xtask | Developer tooling crate. Currently a scaffold." It dispatches four working tasks backed by three modules, two of which other docs already depend on.
- **:33-48** — the Crate Responsibilities table presents itself as complete but omits `hya-sdk`, `hya-native`, and `hya-updater`.
- **:41** — "`hya-plugin-example` | Minimal fixture/example plugin binary." It is `fn main() {}` with 0.0% coverage and implements none of the protocol.
- **:69-71** — repeats the incorrect reducer idempotence claim.
- **:112-129** — the builtin table omits `list_agents` and the five mailbox tools, and mixes runtime planes into the permission-action column.
- **:132** — repeats the 16 KiB output-cap error.
- **:141-145, :153-155** — lists three hya-store files and says "The migration also creates tables for…", as if 0001 were the whole schema; six further modules and seven further migrations exist.
- **:176** — a `team.rs` row with a dead link.
- **:181-182** — repeats the "appends every event through the store and immediately publishes" claim.

### `AGENTS.md`

- **:86** — "`crates/xtask` | Dev-tooling entry point. Currently a small scaffold for future workspace maintenance commands." Same correction as project-structure.md:28.
- **:85** — "`crates/hya-plugin-example` | Minimal plugin binary used as a concrete fixture/example for host and transport behavior." Same correction as project-structure.md:41.
- **:70-88** — the crate table omits `hya-sdk`, `hya-native`, `hya-updater`.
- **:141-145** — the verification commands omit `bun run build` context.

### `DESIGN.md`

- **§2 (palette)** — a single dark palette with every Light cell marked "N/A"; the TUI ships 33 light/dark-aware themes plus a generated `system` theme, with `theme.switch_mode` and `theme.mode.lock`.
- **§4/§5 (Status Line)** — describes a status line carrying "product label, session label, running state, optional YOLO/think/goal state". There is no status line; agent/model/variant/usage live in the prompt footer meta line and no YOLO/think/goal indicator exists.
- **§5 (Prompt Composer)** — "grows from 1 to 6 visible rows" in a 6-11 row `row-input` region; the textarea is capped by `prompt.max_height` (default ⅓ of terminal height, min 6).
- **§5 (Transcript)** — describes "role label followed by wrapped message lines and compact tool rows"; the shipped rendering has agent-colored borders, MIME badges, QUEUED badges, a compaction divider, an assistant footer line, and a revert banner.
- **§6** — "Terminal rendering is immediate; do not add animation artifacts." Animations are pervasive and deliberate (fade-ins, block spinners, an `app.toggle.animations` command).

### `docs/compat-parity.md`

- **:87** — "Skills: `.hya/skills` and `~/.config/hya/skills` discovery … are present." Eleven directories are scanned in a fixed first-name-wins order.
- **:99** — "TUI base | Partial | Ratatui app has compat-dark theme, session picker, permission/question overlays, slash commands, model switching, and render tests." There is no Ratatui app — removed per ADR-0005 and ADR-0010, and `crates/hya/tests/no_rust_tui.rs` asserts the crates do not exist. The default theme is `hya`, not `compat-dark`.
- **:116** — of the `/tui/*` routes: "Missing real TUI main-loop integration and event-bus delivery parity." The frontend consumes `tui.command.execute`, `tui.toast.show`, and `tui.session.select`.
- **:117** — "Missing or incomplete Compat command palette, theme picker/bundled theme library, model variant picker, skill picker error UI, rich markdown/diff/code rendering, usage/cost display wiring, prompt stash, and full keymap/leader UX." **All** of these ship. Rewrite the row to whatever genuinely remains.
- **:113** — "Global config is runtime-only" buried in a paragraph; promote the PATCH-replaces-whole-object and never-persisted facts (or cross-link the new server-client.md subsection).

### `docs/opencode-feature-inventory.md`

- **:16** — "richer file refs, prompt UX, theme/model pickers, undo/redo UI need coverage." `@` completion, the theme picker, the model picker with favorites/recents, and the undo/redo UI with revert banner and Confirm Redo dialog all ship.
- **:17** — "TUI config and attention: partial … full dedicated config and attention behavior need scope decision." Both the validated TUI config schema and the attention service with notifications, focus gating, and a six-slot sound pack ship. No scope decision is outstanding.

### `docs/hya-pi-compat-comparison.md`

- **:215-217** — "`TeamControlPlane` models lifecycle, mailbox, and task-board state" — deleted per ADR-0001; only the `WorktreeManager` half of the sentence survives.
- **:220-221** — "subagents cannot recursively spawn more subagents through `TaskTool`". Nested spawning is supported and **bounded**: `max_depth = 5`, depth-based reserved/general permit selection, and `AdmissionMemberIdentity` propagated through a `task_local` specifically so a nested spawn is attributed to its admitted parent member.
- **:336** — repeats the `**/plugin.toml` recursive-glob error.

### Source-file rustdoc that is stale

- **`crates/hya-proto/src/ids.rs:457`** — "Monotonic per-session event sequence (the `event_log.seq` rowid)." It is **globally** monotonic; also omits that seq 0 is the live-only sentinel.
- **`crates/hya-sdk/src/reducer.rs:3-6`** — "Today it is a no-op skeleton" — `apply()` is implemented.
- **`crates/hya-core/src/engine/mailbox.rs:9-12`** — "idle sessions are NOT woken (that is Phase 4…)". Phase 4 shipped: `ResidentSupervisor::run_bus` parks on the bus and routes every `MailSent` to its team. Source: `crates/hya-core/src/resident.rs:1361`.
- **`crates/hya-app/src/lib.rs` crate doc** — the trailing "Public surface filled in during Phase 1" is stale; drop it.
- **`crates/hya-core/src/lib.rs` crate doc** — the "team orchestration and completion engines land in later phases" clause is stale; completion.rs, loop_mode.rs, orchestrator.rs and subagent.rs all exist and are exported.
- **`crates/hya-plugin/src/lib.rs` crate doc** — the trailing "Phase 0 ships the crate skeleton only" no longer matches the crate.
- **`crates/hya-plugin-example/src/main.rs:1-5`** — the `//!` describes Phase 7 behaviour the stub does not implement; it is aspirational, not accurate.
- **`crates/hya-client/src/lib.rs:1`** — "`hya-client` — typed HTTP client for the hya server (used by the TUI)." The "(used by the TUI)" consumer is the removed Rust TUI; re-state the real consumer or record that the crate currently has none.
- **`crates/hya-server/src/lib.rs:1`** — cites "design.md §11"; verify the cross-reference is still valid.

---

## Rustdoc work

Per crate: whether a crate-level `//!` is needed, undocumented module-level public items, and the named priority items to document first.

| Crate | Needs crate `//!`? | Undocumented / total | Priority items |
| --- | --- | ---: | --- |
| `hya` | **Yes** — no `//!` at all | 0 / 0 | Add a `//!` explaining the binary is an `argv[0]`-preserving trampoline that `exec()`s `hya-ts`; that is the whole point of the crate and is non-obvious. `crates/hya/src/main.rs:1` |
| `hya-app` | No (exists; drop the stale "Public surface filled in during Phase 1") | 22 / 74 | `HyaRuntime` (runtime.rs:4267), `HyaRuntime::start` (:4276), `RuntimeOptions` (:4259), `build_session_engine` (:3885), `RuntimeConfig` (:1218), `open_store` (:1321), `ResolvedConfig` (config.rs:30), `spawn_team_supervisor` (runtime.rs:3318), `ModelEntry` (config.rs:46), `spawn_reject_responder` (permission.rs:5). Also 9/25 undocumented inherent methods incl. the whole `HyaRuntime` accessor set (:4342 router, :4347 engine, :4352 app_state) and `RuntimeConfig::with_yolo` (:1237) |
| `hya-client` | Rewrite (consumer clause is stale) | 2 / 2 module-level; **6 / 6** counting impl methods | `Client` (lib.rs:12), `ClientError` (:7), `Client::new` (:19), `create_session` (:26), `prompt` (:42), `events` (:60 — needs error and termination semantics) |
| `hya-server` | No (thin; verify the design.md §11 link) | 4 / 4 | `router` (lib.rs:33 — the crate entry point; document the route set and the CORS policy it installs), `AppState` (state.rs:12), `AppState::new` (:25), `ApiError` (lib.rs:54), trait `McpControl` (mcp_control.rs:7 — a public extension point with an undocumented contract). Also the five undocumented `AppState` builder methods (:47-72) that sit beside the one documented `with_default_agent` (:41) |
| `hya-core` | No (exists; drop the "later phases" clause) | **74 / 111** — worst by volume | `SessionEngine` (engine.rs:228) and its `new` (:253), the whole `with_*` builder chain (:302-373) and all accessors (:384-419); `EventBus` (bus.rs:11); `CreateSession` (engine.rs:87); `AgentSpec` (:95); `CoreError` (error.rs:6); **every public extension trait** — `HookDispatcher` (hooks.rs:13), `RuntimeCatalogRefresh` (engine.rs:149), `Summarizer` (compaction.rs:107), `IterationGate` (completion.rs:52), `IterationExecutor`, `GoalEvaluator`, `LoopVerifier` (loop_mode.rs:36), `LoopPlanner`, `RuntimeSourceOwner`; the 13-type hooks.rs Input/Outcome family (:67-139); `RuntimeSource` (runtime_registry.rs:75). Impl methods 128/169 undocumented — per-file: engine.rs 34, runtime_registry.rs 33, engine/session_state.rs 14, engine/admission.rs 10, workspace.rs 8 |
| `hya-backend` | No | 17 / 18 | `Cli` (cli_args.rs:14) and `Command` (:58) — highest value because clap doc comments double as `--help` text; `cmd_serve` (serve.rs:8); `RpcRequest` (rpc.rs:2); `parse_rpc` (:8); `first_run_config_bootstrap` (main.rs:41); `AuthCommand` (auth_cmd.rs:9); `BundleCommand` (bundle_cmd.rs:15) |
| `hya-store` | No (crate doc is substantive) | **31 / 31 — 0% coverage** | `SessionStore` (lib.rs:41 — the crate's entire entry point), `StoreError` (error.rs:8), `SessionInfo` (lib.rs:46), `LedgerEntry` (:54), `AdmissionState` (admission.rs:16), `AdmissionRecord` (:99), `AdmissionClaimOutcome` (:116), `AdmissionTerminal` (:134), `BundleRegistry` (bundle_registry.rs:11), `MAX_ADMISSION_INTENT_BYTES` (lib.rs:16). Also 9/9 undocumented `pub fn` in lib.rs impl blocks |
| `hya-proto` | No (strong) | 23 / 48 — best of the non-SDK crates | `Event` (event.rs:20 — the enum the crate exists to define), the whole `Projection` family (projection.rs:189, :23, :75), `Part` (message.rs:153), `TokenUsage` (:94), `Role` (:11), and every api.rs request/response struct (`CreateSessionRequest` :7, `PromptRequest` :21). Macro-generated ids excluded |
| `hya-sdk` | Expand (one line; does not relate the crate to hya-server/hya-native) | **0 / 37** | None outstanding. Fix the stale `reducer.rs:3-6` module doc |
| `hya-provider` | No | 30 / 38 | The three defining traits `Provider` (lib.rs:341), `Protocol` (:374), `Decoder` (:379) — anyone adding a backend implements them and gets no contract text on ordering, error semantics, or SSE-framing ownership; `CompletionRequest` (:322); `Capabilities` (:63); `ProviderError` (:46); `ProviderRouter` (router.rs:10, 1/8 impl methods); `HttpProvider` (http.rs:80); `ProviderKind` (:31); `EventStream` (lib.rs:43) |
| `hya-tool` | Expand (one line for the largest surface in the workspace) | **134 / 147 — ~9%** | Cluster 1, tool.rs core: `Tool` trait (:134), `ToolCtx` (:72), `ToolError` (:39), `ToolPermission` (:185), `ResolvedTool` (:194) — the first five things a tool author touches, while the neighbouring `ToolRegistry`/`ToolRegistrySnapshot` are already well documented. Cluster 2, permission.rs security state machine: `PermissionPlane` (:420), `PermissionRules` (:306), `Invocation` (:129), `InvocationPolicy` (:184), `PermissionInterceptor` (:405), plus Action, Resource, Mode, PermissionModel, InvocationRule, Decision, RememberScope. Cluster 3, the plane pattern: `SpawnerPlane` (spawn.rs:123), `InteractionPlane` (interaction.rs:131), TodoPlane, LspPlane, MailboxPlane, SkillPlane, WebSearchPlane. The ~30 concrete `*Tool` structs are lower priority (behaviour carried by their JSON schema) |
| `hya-mcp` | **Yes** — `lib.rs` has no `//!` at all | 17 / 18 | `McpServerConfig` (manager.rs:14), `prepare` (:79), `McpClient` (client.rs:42), `McpError` (:22), `PreparedMcpServer` (manager.rs:54), `McpStatus` (:27), `McpTool` (bridge.rs:14), `namespaced_tool_name` (:47), and the entire 6-item `protocol.rs` wire module (JsonRpcRequest/Response/Error, ToolInfo, ToolsListResult, ToolCallResult). Only `McpManager` (manager.rs:44) is documented |
| `hya-plugin` | No (exists; drop the stale "Phase 0 ships the crate skeleton only") | **60 / 62** | `PluginHost` (host.rs:40), `PluginClient` (client.rs:64), `PluginSpec` (config.rs:29), `PluginEntry` (:15), `Manifest` (manifest.rs:12), `HookName` (messages.rs:23), `PROTOCOL_VERSION` (:14), `PluginStatus` (host.rs:33), `PermissionBridge` (permission_bridge.rs:20), `Frame` (protocol.rs:102). messages.rs alone contributes ~33 undocumented wire types — the ABI an external plugin author reads first. protocol.rs is 5/5 undocumented. 32/46 impl methods undocumented |
| `hya-plugin-compat` | **Yes** — no `//!` at all | 2 / 2 | `COMPAT_PLUGIN_VERSION` and `COMPAT_SDK_VERSION` (lib.rs:1,3) — say what the pin means and what breaks when it moves. Highest doc value per line in the workspace: three doc comments complete the crate |
| `hya-plugin-example` | Rewrite (aspirational, not accurate) | 0 / 0 | Correct the `//!` to describe the stub as a stub |
| `hya-bundle` | No (good) | 24 / 31 | `prepare_package` (prepare.rs:25) and `stage_package` (package.rs:133) — the two entry points an external caller hits first, the latter ~150 lines of staging/cleanup with non-obvious failure semantics; `PreparedBundle` (model.rs:111), `PreparedCatalog` (:135), `PreparedAgent` (:80), `BundleSource` (source.rs:33), `PackageInspection` (package.rs:79), `PackageFormat` (:46), `cleanup_orphaned_staging` (:90). 18/21 impl methods undocumented |
| `hya-native` | No (best-in-group) | **0 / 3** | None |
| `hya-updater` | No (strongest crate doc in the workspace) | 1 / 37 | `read_floor` (journal.rs:193) — security-adjacent (the anti-rollback floor), so the absence matters more than the count |
| `hya-e2e` | No (solid) | 3 / 26 | `ToolCallStep` (fake_llm.rs:29), `tool_step` (:324), `tools_step` (:331) — exactly what a test author reaches for first. **Also remove or scope `#![allow(missing_docs)]` in lib.rs**, which disables the lint that would catch regressions |
| `hya-ts` | **Yes** — the `//!` leads with the crate name and restates it; `main.rs` has none | 3 / 12 | `Cli` (lib.rs:10, with ~10 undocumented pub fields), `BunCommand` (:275), `invocation_name` (:41). Rewrite the `//!` to explain the launcher shim role: resolve and spawn the Bun TUI frontend against a hya backend |
| `xtask` | **Yes** — "Dev tooling entrypoint." explains nothing | 0 / 2 | Rewrite the `//!` to name the dispatched tasks (matrix_check, startup_bench, sync_compat) and how they fit the repo workflow. Both public items are already documented; note the whole `sync_compat` subtree is crate-internal and unreachable from outside |

---

## Sequencing

Batches are file-disjoint: no two batches touch the same file, so all batches within a wave can run in parallel with a distinct writer each. Within a batch, one writer owns every listed file.

### Wave 1 — independent content batches (16 parallel writers)

| Batch | Owner writes | Notes |
| --- | --- | --- |
| **A** | `docs/configuration.md` | Largest single file (~60 bullets). Writes only a **pointer stub** for Skills — the authoring content belongs to Batch L. Writes only a **pointer** for the TUI slash-command list — the table belongs to Batch J. Owns the TUI env-var reference table; Batch I writes behaviour prose that links here. |
| **B** | `docs/cli.md` | Owns the exit-code section, serve signal handling, bare-`hya-backend`, `--db` correction, all OAuth/model CLI additions, and bundle staging. Writes a **short list + link** for slash commands; the table belongs to Batch J. |
| **C** | `docs/architecture/providers.md` | Includes the new Responses Protocol, Capabilities, Configured Identity, Usage Reporting sections and all corrections in this file. |
| **D** | `docs/architecture/runtime.md` | Includes the turn-loop rewrite, compaction rewrite, hook-list correction, `team.rs` removal, and all engine-seam sections. |
| **E** | `docs/architecture/event-model.md` | Full Event catalog, EventSeq/`Projection::apply` corrections, live-vs-durable streaming, permission ask lifecycle. |
| **F** | `docs/architecture/storage.md` + `docs/architecture/admission-and-governor.md` (new) | Paired because the admission schema and the admission API must agree; one writer avoids drift. |
| **G** | `docs/architecture/tools-and-permissions.md` + `docs/architecture/agent-tool-surface.md` | Paired because they currently contradict each other on `write`/`edit` schemas, the builtin inventory, and the `skill` row; one writer reconciles both. Includes the anchor-drift fix. |
| **H** | `docs/architecture/server-client.md` | Permissions routes, provider catalog, `/config` runtime-only note, model-ref object form, DTO bodies, ApiError statuses. |
| **I** | `docs/architecture/tui.md` | Terminal handoff, argv contract, owned backend lifecycle, subagent workspace + run tree, agent visibility, dialog primitives, frontend plugin host, renderer, control events, startup navigation, resolution orders, enforced boundaries, package exports, env-flag behaviour prose (links Batch A's table). |
| **J** | `docs/tui-keybindings.md` (new) + `docs/tui-reference.md` (new) | Paired: both derive from the same keymap source and must not duplicate each other. Canonical home for slash commands and every screen/dialog. |
| **K** | `docs/plugin-protocol.md` (new) + `docs/compat-plugins.md` (new) | Paired: the compat adapter implements the protocol; one writer keeps the hook vocabulary consistent across both. |
| **L** | `docs/agent-bundle-authoring.md` + `docs/skills.md` (new) | Paired: bundle `resources.skills` and skill discovery/frontmatter overlap; one writer owns both. Canonical home for Skills. |
| **M** | `docs/self-update.md` | Metadata validation, trust roots, staging/smoke, recovery rules, discard guardrails, skills back-link. |
| **N** | `docs/getting-started.md` + `docs/troubleshooting.md` + `docs/development.md` + `docs/testing/process-e2e.md` | Four small user-facing files, no overlap with any other batch. Includes install.sh docs, startup-trace troubleshooting, xtask, frontend build/tooling, automation hooks. |
| **P** | `docs/adr/0001-*.md` + `docs/adr/0002-*.md` + `docs/adr/0003-*.md` + `docs/adr/0006-*.md` + `CONTEXT.md` | ADR corrections plus the CONTEXT.md "every current subscriber" phrasing that ADR-0001 also fixes. |
| **R** | `packages/hya-tui-ts/README.md` (new) + `scripts/README.md` (new) + `test/README.md` (new) | Package-local docs; touches no repo docs. |

### Wave 2 — rustdoc (12 parallel writers; crates never overlap files)

Each crate is a separate writer. Order within the wave is irrelevant; grouped here by priority.

| Sub-batch | Crates | Rationale |
| --- | --- | --- |
| **Q1** | `hya-tool` | 134 undocumented items, security-relevant permission model. Largest single job — assign alone. |
| **Q2** | `hya-core` | 74 module-level + 128 impl methods. Assign alone. |
| **Q3** | `hya-store` | 0% coverage, 31 items including the whole admission state machine. |
| **Q4** | `hya-plugin` + `hya-plugin-compat` + `hya-plugin-example` | One extensibility-ABI writer; `hya-plugin-compat` is a 3-line file and `hya-plugin-example` is a one-line `//!` fix. |
| **Q5** | `hya-provider` | The three defining traits plus 27 other items. |
| **Q6** | `hya-proto` | 23 items, mostly `Event` and the Projection family. Must land the `ids.rs:457` EventSeq correction. |
| **Q7** | `hya-app` + `hya-backend` | Paired: `HyaRuntime`/`RuntimeOptions` and the CLI surface are adjacent concerns; 39 items combined. |
| **Q8** | `hya-mcp` | Needs a crate `//!` from scratch plus 17 items. |
| **Q9** | `hya-bundle` | 24 items; pairs conceptually with Batch L's authoring guide but touches different files, so it may run in Wave 1 or 2. |
| **Q10** | `hya-server` + `hya-client` | Small; `hya-client` needs its stale consumer clause rewritten. |
| **Q11** | `hya-sdk` (reducer.rs module doc) + `hya-core/src/engine/mailbox.rs` module doc | **Conflict warning:** `hya-core/src/engine/mailbox.rs` is also touched by Q2. Assign the mailbox module doc to Q2 and give Q11 only `hya-sdk/src/reducer.rs`, or fold Q11 into Q2. |
| **Q12** | `hya-ts` + `xtask` + `hya` + `hya-updater` + `hya-e2e` | Five small jobs: three crate-level `//!` rewrites, `read_floor`, three fake_llm builders, and removing `#![allow(missing_docs)]` from hya-e2e. |

### Wave 3 — TSDoc (3 parallel writers)

| Batch | Owner writes |
| --- | --- |
| **S1** | `src/main.tsx`, `src/hya/platform.ts`, `src/hya/product.ts`, `src/hya/audit.ts`, `src/hya/static-host.ts`, `src/hya/sdk-spine.tsx`, `src/hya/startup-trace.ts` — the hya-owned surface. |
| **S2** | `src/upstream/routes/session/subagent-workspace.ts`, `src/upstream/routes/session/index.tsx` — the two largest hya-authored files under `src/upstream`. **Must not run concurrently with Batch I or J**, which cite these files but do not edit them; no conflict in practice. |
| **S3** | `src/upstream/feature-plugins/system/diff-viewer-file-tree-utils.ts`, `src/upstream/config/keybind.ts`, `src/upstream/keymap.tsx`, `src/upstream/config/index.tsx` — 72 exports, 0 docblocks. |

### Wave 4 — cross-reference reconciliation (single writer, runs last)

One writer owns every file whose correctness depends on the new documents existing:

- `docs/README.md` — add `docs/plugin-protocol.md`, `docs/compat-plugins.md`, `docs/skills.md`, `docs/tui-keybindings.md`, `docs/tui-reference.md`, `docs/architecture/admission-and-governor.md` to the Docs Map and the reading paths.
- `README.md` — repoint :111 at `docs/tui-keybindings.md`, scope the :79-80 completeness claim, fix the :74-77 `hya-ts` block.
- `AGENTS.md` — crate table additions, xtask and plugin-example corrections, `bun run build` context.
- `docs/project-structure.md` — all crate/module map corrections, the output-cap and reducer fixes, the builtin table, the new package README link.
- `DESIGN.md` — §2, §4, §5, §6 corrections.
- `docs/compat-parity.md` — :87, :99, :113, :116, :117.
- `docs/opencode-feature-inventory.md` — :16, :17.
- `docs/hya-pi-compat-comparison.md` — :215-217, :220-221, :336.

Wave 4 must run after Waves 1–3 so every cross-link it writes resolves. Everything else is fully parallel.