# TUI structure (`tui.toml`)

The TUI draws its screen from components: the transcript and the message
input are always there; a status line, the sidebars, and the layout tree are
optional. `tui.toml` picks which ones you see and how. It is declarative: the
TUI only reads it and never runs code from it. Custom rendering belongs to
[TUI extension bundles](tui-extensions.md). The decision record is
[ADR-0036](adr/0036-tui-component-presets-and-tui-toml.md).

Without a `tui.toml` the TUI uses the `statusline` preset: the transcript, the
input, and one status row below the input with the model, the permission
mode, and the context occupancy, at any width:

```text
 ┃ fix the failing test

 ● hya-main · openai/gpt-6.1
 The test failed because …

┌────────────────────────────────────────────────────────────────────────┐
│ Message, !shell, or @file · / commands                                 │
└────────────────────────────────────────────────────────────────────────┘
 gpt-6.1 · high · manual · hya · ⎇ main · ctx ━━─── 42% · 42.3k tok
```

Use the `workspace` preset for the tiled workspace with the Projects,
Sessions, Todos, and Context sidebars ([Layout](tui.md#layout)).

## Presets

| Preset | Draws |
| --- | --- |
| `minimal` | Transcript and input. The model and the permission mode are written on the input's top border; no status row. |
| `statusline` (default) | Transcript, input, and the status row below it. |
| `workspace` | The tiled workspace: Projects on the left; Sessions, Todos, and Context on the right from 150 columns; the layout tree, `/layout`, the layout editor, and drag resizing. Below 150 columns, or while `/sidebar off` hides Context, the status row returns. |
| `dock` | Reserved for the status band with todo and subagent rows (a later version); selecting it warns and uses `statusline`. |

| Component value | `minimal` | `statusline` | `workspace` |
| --- | --- | --- | --- |
| `startup.enabled` | `true` | `true` | `true` |
| `startup.resources` | `"compact"` | `"compact"` | `"compact"` |
| `statusline.enabled` | `false` | `true` | `true` |
| `statusline.segments` | `["model", "mode", "cwd", "git", "ctx", "tokens"]` | same | same |
| `statusline.placement` | `"below"` | `"below"` | `"below"` |
| `composer.style` | `"box"` | `"box"` | `"box"` |
| `composer.border_info` | `["model", "mode"]` | `[]` | `[]` |
| `composer.paste.collapse_lines` | `10` | `10` | `10` |
| `composer.paste.collapse_chars` | `1000` | `1000` | `1000` |
| `composer.paste.wrap` | `true` | `true` | `true` |
| `composer.paste.image_resize` | `true` | `true` | `true` |
| `sidebar.enabled` | `false` | `false` | `true` |

Bundle TUI panels with `placement: "sidebar"` join the workspace tree; in the
single-column presets they appear once the sidebar component is on
(`/set sidebar on`; [TUI extensions](tui-extensions.md)).

## Usage

Pick a preset:

```toml
# ~/.config/hya/tui.toml
preset = "workspace"
```

Change one component of the default:

```toml
[statusline]
segments = ["model", "mode", "cwd", "git", "ctx"]

[composer.paste]
collapse_lines = 20     # fold pastes longer than 20 lines into a chip
```

Define your own preset on top of a built-in one:

```toml
preset = "focus"

[presets.focus]
extends = "minimal"
statusline = { enabled = true, segments = ["model", "mode", "ctx"] }

# Top-level tables override the preset in effect.
[statusline]
segments = ["model", "ctx"]
```

`/config init` writes a commented template that lists every preset and key.

### File location

The first that applies:

1. `$HYA_TUI_TOML`
2. `tui.toml` in the directory of `$HYA_TUI_CONFIG` (tests and profiles that
   move `tui.json` move this file with it)
3. `$XDG_CONFIG_HOME/hya/tui.toml`
4. `~/.config/hya/tui.toml`

The file belongs to the machine that runs the TUI: in the WebUI that is the
PTY host, and with a remote backend it is still this machine. There is no
project-level `.hya/tui.toml`.

### Precedence

Later layers win:

built-in preset → named presets along the `extends` chain (outermost first) →
the top-level component tables → `/set` for this session.

Tables merge key by key; arrays replace. `enabled = false` turns a component
off and keeps its other keys, so `/set <component> on` brings it back as
configured.

### Commands

| Command | Effect |
| --- | --- |
| `/config` | Show the file path, the preset (and its `extends` chain), every component key with its value and the layer that set it (`preset <name>`, `tui.toml`, `this session`), and every warning. |
| `/config reload` | Read `tui.toml` again and apply it now; the draft and the open session stay. |
| `/config edit` | Open `tui.toml` in `$VISUAL`/`$EDITOR` (writing the template first when it is missing), then reload. |
| `/config init` | Write the commented template when the file does not exist; refuse otherwise. |
| `/set preset <name>` | Use another preset for this session; the file is not written. A preset switch drops earlier `/set <component>` changes. |
| `/set <startup\|statusline\|sidebar> on\|off` | Turn a component on or off for this session. |
| `/resources` | Append the full startup list (every section, grouped by source, `none` for an empty one) to the transcript. |
| `/sidebar`, `/projects-sidebar` | In a preset without sidebars, `on` (or a toggle) is `/set sidebar on`; in the workspace they keep their usual meaning ([Layout](tui.md#layout)). |

None of these has a default shortcut, and all are slash commands, so they work
in the WebUI and in specs (`press()`).

### Diagnostics

A configuration error never blocks startup.

| Problem | Handling |
| --- | --- |
| No file | The default preset, no notice. |
| TOML syntax error | The whole file is ignored (`Ignored <path> (line N): <error>; using the statusline preset`). |
| Unknown key, wrong type, unknown segment | That item is ignored with a warning; the lower layer's value stays. |
| Unknown preset, `extends` cycle, more than 8 presets in a chain, a `[presets.*]` named like a built-in | The default preset with a warning. |
| A key or value of a later version (`placement = "band"`, `[startup]`, `[composer.attachments]`, `[theme]`, `[keys]`, …) | Ignored: `… is not supported in this version; ignored`. |
| `version` above 1 | A warning; the keys this version knows still apply. |

Warnings appear at the start of the transcript, in the startup list, under
`[Config] <path> · N warnings · /config lists them`, and scroll away with the
history; `/config` lists them all. They show even with `startup.enabled = false`
or `quiet = true`.

**Upgrade notice.** A TUI that finds a saved `paneLayout` in `tui.json` and
no `tui.toml` shows once, at the start of the transcript:
`[Layout] The default layout is now statusline. For the sidebars, write
preset = "workspace" in tui.toml …`. It records `"statusline-default"` in
`tui.json` `seenNotices` and never switches the preset by itself.

## Startup list

The transcript starts with what this TUI and its backend loaded, one line per
section, sorted by name and cut with `+N` at the width:

```text
 [Skills] agent-bundle-authoring, brainstorm, debug, +11
 [Slash commands] /clear, /help, /init, /model, /review, /sessions, /workflow
 [MCP] files, slack
   slack: failed: MCP connection closed · /mcp to fix
 [Bundles] hya/agent-channels, hya/base-tools, hya/code-mode (disabled), +9
 [Extensions] hya/basic-tui-components
```

| Section | Source | Problems (warning color, shown with `quiet` too) |
| --- | --- | --- |
| `[Config]`, `[Layout]` | `tui.toml` warnings and the one-time default-change notice | Always |
| `[Context]` | `Bootstrap.contextFiles`: the `AGENTS.md` files a session here reads, parent first; the full list groups them by `project`, `parent`, `user` with sizes | — |
| `[Skills]` | `Bootstrap.skills`; the full list groups them by `source` (`builtin`, `bundle`, `project`, `user`) and skill directory | — |
| `[Slash commands]` | Custom commands (`CommandSummary.source = command`) | — |
| `[MCP]` | `GetMcpStatus`, asked again every 0.5 s while a server is still connecting, for at most 10 s; then once | A failed server, a server that needs a login |
| `[Bundles]` | `ListBundles`; `(disabled)` and `(shadowed)` are marked, not problems | An unreadable bundle |
| `[Extensions]` | The TUI extensions of this scope | A failed or blocked extension, extension warnings, an unreadable extension catalog |

A section appears once its data arrived; an empty one has no line.
`resources = "full"` lists each section grouped by source with versions and
states, `sections` picks and orders the sections, and `quiet = true` keeps
only the problems. The list scrolls away with the history and stays at the top
of the transcript when you switch sessions. `/resources` prints the full list
again at the end of the transcript, for example after `/reload`. It is display
only: no event is written.

## Status line

One borderless row below the input. Segments are drawn left to right in the
configured order and separated by ` · `; `session` sits at the right end.

| Segment | Shows | When the width is short |
| --- | --- | --- |
| `model` | The session's model id and thinking effort, `gpt-6.1 · high` | Never dropped; the name is cut last |
| `mode` | The permission mode; `⚠ yolo` in the error color, a bundle mode in the focus color | Never dropped |
| `ctx` | Context occupancy, a five-cell bar and the percent (`ctx ━━─── 42%`); the warning color from 80 %, the error color from 95 % | Never dropped |
| `connection` | `reconnecting` (warning) or `backend stopped` (error); nothing while connected | Never dropped |
| `agent` | The session's agent | Dropped fifth |
| `cwd` | The active Project's name, else the session directory (`~` for home) | Dropped fourth |
| `git` | `⎇ <branch>` | Dropped third |
| `tokens` | The session's token total, `42.3k tok` | Dropped second |
| `session` | The session title or id, right-aligned | Dropped first |

A segment without data is left out: `ctx` until a round reported usage against
a known context limit (the same rule as the Context pane,
[Working indicator, metadata state, and todo panel](tui.md#working-indicator-metadata-state-and-todo-panel)),
`git` outside a repository, `tokens` while the total is zero. The status line
is display only: it reads the store's session, catalog, and workspace fields
and appends no events.

`composer.border_info` writes the given segments (same names, no elision, cut
to the border width) on the input's top border, right-aligned; `! shell`
replaces them while the input holds a shell command.

**Deduplication.** The same fact is shown in one resident place. In the
workspace preset the status line collapses while the Context pane is visible
and returns when the width or `/sidebar off` hides it.

## Interface definitions

### Schema (version 1)

| Key | Type | Default (`statusline`) | Notes |
| --- | --- | --- | --- |
| `version` | integer | `1` | Reserved for migrations. |
| `startup.enabled` | boolean | `true` | `false` shows only the `[Config]`/`[Layout]` notices. |
| `startup.resources` | `"compact"` \| `"full"` | `"compact"` | One line per section, or grouped by source. |
| `startup.sections` | array of section names | `["context", "skills", "commands", "mcp", "bundles", "extensions"]` | Which sections, in this order; `commands` is `[Slash commands]`. |
| `startup.quiet` | boolean | `false` | Only problem items. |
| `preset` | string | `"statusline"` | A built-in preset or a `[presets.*]` name. |
| `presets.<name>.extends` | string | `"statusline"` | The preset this one starts from; at most 8 named presets per chain; no cycles. |
| `presets.<name>.<component>` | table | — | Same keys as the top-level component table. |
| `statusline.enabled` | boolean | `true` | |
| `statusline.segments` | array of segment names | `["model", "mode", "cwd", "git", "ctx", "tokens"]` | `model`, `mode`, `ctx`, `connection`, `agent`, `cwd`, `git`, `tokens`, `session`; duplicates are dropped. |
| `statusline.placement` | `"below"` \| `"band"` | `"below"` | `"band"` is reserved for a later version. |
| `composer.style` | `"box"` \| `"band"` | `"box"` | `"band"` is reserved for a later version. |
| `composer.border_info` | array of segment names | `[]` | |
| `composer.paste.collapse_lines` | integer ≥ 0 | `10` | A text paste of more lines becomes a paste chip ([Paste and attachments](tui.md#paste-and-attachments)); `0` never folds. |
| `composer.paste.collapse_chars` | integer ≥ 1 | `1000` | Or of more characters. |
| `composer.paste.wrap` | boolean | `true` | Send paste and file chips as `<attachment>` blocks; `false` sends their raw text. |
| `composer.paste.image_resize` | boolean | `true` | Convert a pasted image the server refuses (BMP, TIFF, HEIC, AVIF) to PNG and scale one over 10 MiB down (`Bun.Image`); `false` refuses them. |
| `sidebar.enabled` | boolean | `false` | `true` draws the tiled workspace (the saved `paneLayout`, or the default tree). |

Keys of later versions (`todos.*`, `subagents.*`, `layout.*`,
`transcript.*`, `sidebar.left`, `sidebar.right`, `sidebar.min_columns`,
`composer.attachments.*`) are recognized and ignored with "not supported in
this version".

### Reserved tables

`[theme]` and `[keys]` are not part of version 1; a v1 TUI warns and ignores
them. The intended contract mirrors `tui.json`:

```toml
[theme]
name = "ember"            # hya | light | contrast | ember

[keys.workspace]           # a CustomKeybindings scope
"alt+s" = "/sessions"
"f6" = false               # false disables the key (TOML has no null)
```

`"key" = "command"` in `[keys.<scope>]` will map to
`keybindings[key] = { command, scope }`, and `false` to `null`. Values fixed by
`tui.toml` will win over `/theme` and `/keybind set`, which then change them
for the session only.

### `tui.toml` and `tui.json`

| File | Written by | Holds |
| --- | --- | --- |
| `tui.toml` | You (and `/config init` when missing) | Structure: the preset and component keys. |
| `tui.json` | TUI commands | State: theme, vim, notifications, permission mode, keybindings, extension settings, `paneLayout` (used only by the workspace structure), `seenNotices`. |

The two files share no keys. Only the workspace structure reads or writes
`paneLayout`: the single chat column of the other presets is never saved, and
`/layout` editing commands (`split`, `insert`, `move`, `reset`, `reload`, …)
are refused there with `The layout tree belongs to the workspace preset`;
`/layout show` and `/layout focus` still work.

### Code

| Module | Contract |
| --- | --- |
| `packages/hya-tui/src/config/presets.ts` | `TuiStructure`, `StatusSegment`, `builtinPreset(name)`, `availablePresets`, `defaultPreset`. |
| `packages/hya-tui/src/config/tuiConfig.ts` | `tuiConfigPath(env)`, `loadTuiConfig(path)`, `resolveTuiConfig(raw, session)` → `{ preset, presetSource, chain, structure, sources, warnings }`, `tuiConfigTemplate()`, `initTuiConfig(path)`. Never throws on bad input. |
| `packages/hya-tui/src/app/structure.ts` | `createStructureController({ store, path, preferencesPath, editFile })`: applies the structure, the start-of-transcript notices, and the pane tree to the store; `setPreset`, `setEnabled`, `reload`, `init`, `edit`, `describe`. |
| `packages/hya-tui/src/state/startup.ts` | `startupLines(input, { resources, sections, quiet }, width)` → `{ text, tone }[]`; `compactLine`, `skillGroup`. Pure. |
| `packages/hya-tui/src/components/StartupBlock.tsx` | The startup list at the start of the transcript. |
| `packages/hya-tui/src/state/statusline.ts` | `statusPart(state, id)`, `fitStatusLine(parts, columns)`, `statusLineShown(state, columns)`, `borderInfoText(...)`. Pure. |
| `packages/hya-tui/src/components/StatusLine.tsx` | The status row, drawn by the message pane below the input. |

The structure is local to the TUI: no `hya.v1` route, event, or preference
key of the backend changes.
