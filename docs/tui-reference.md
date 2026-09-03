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
  - **Workflow** — selected Workflow/revision availability, run status, graph level, declaration-ordered active Stages, Agent and Stage progress, and bounded current work.
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

**Observation pane.** Read-only sticky-bottom transcript. A single header
string (one word-wrapped `<text>`, optional Working spinner beside it) is built
by joining non-empty parts with ` - `:

```text
handle - agent_type - Working|Finished|Failed|Cancelled|Idle - task - placement - focused|open - read-only [- <nav hint>]
```

The trailing nav hint is **not** a second line. It is appended only when the
pane is focused:

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

Tool-specific titles cover **bash** (icon `#`, title falls back to `Shell` when
the permission payload has no description — the closed Bash schema does not
accept a model-authored description; body `$ <command>`), Edit, Read, Glob,
Grep, List, WebFetch, WebSearch, Task, external-directory access, repeated
failure, and a generic tool call. A stale runtime `shell` request uses the same
Bash permission presentation and is not a second UI surface.

**Allow always** opens a Confirm/Cancel stage warning that the patterns will be
allowed until hya is restarted. **Reject** on a **child** session (session has
`parentID`) opens a free-text **Reject permission** editor: Return confirms with
the message, Escape cancels back. On a **root** session, Reject replies
immediately with `reply: "reject"` and no message editor.

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

The hya-owned coding-tool renderer consumes projected SDK `ToolPart` state.
SyncProvider is the only Session message/part owner: presentation does not
fetch/poll the backend, replay raw Events, hydrate a second message store, or
schedule a timer. Completed coding results cross one semantic boundary:

- model-facing `output` is bounded text for the next provider round;
- host-facing `metadata` is a bounded, allowlisted payload for titles, line
  ranges, syntax facts, diffs, diagnostics, truncation, and command status.

Unknown input keys and `env` values never enter the view. Malformed, compacted,
or cap-collapsed data uses the existing readable inline/error fallback instead
of rendering arbitrary JSON. Pending, streaming, permission, denied,
attachment, directory, diagnostic, and generic states keep their existing
fallback behavior. Initial hydration, one live `message.part.updated`
replacement, and a replayed completed `ToolPart` use this normalizer once.
Top-level `truncated` and the backend `outputTruncated`, `titleTruncated`,
`displayTruncated`, `diffTruncated`, `attachmentsTruncated`,
`diagnosticsTruncated`, `warningsTruncated`, `rowsTruncated`,
`groupsTruncated`, `metadataTruncated`, `unknownFieldsDropped`, and
`envelopeTruncated` facts are combined; local collapse cannot erase any
backend truncation fact.

Dedicated renderers (via `toolDisplay()`) cover `bash`, `glob`, `read`, `grep`,
`webfetch`, `websearch`, `write`, `edit`, `task`, `apply_patch`, `todowrite`,
`question`, and `skill`. Everything else is **GenericTool**.

| Tool | Pending form | Completed form and metadata |
| --- | --- | --- |
| `read` | Inline `→` + path, `Reading file...` | **Structured block** titled by path; file-derived syntax highlighting, stable source line numbers and requested offset, bounded expand/collapse. Directory and media attachment results keep their existing readable/attachment views. |
| `write` | Inline `←`, `Preparing write...` | **Structured block** titled by path with final post-formatter text, file-derived syntax highlighting, stable line numbers, diagnostics, and bounded expand/collapse. |
| `edit` | Inline until a valid diff is available | **Structured block** using the semantic diff primitive and final-state metadata; unified or split according to width and `diff_style`. |
| `grep` | Inline `✱` + pattern/path, `Searching content...` | **Structured per-file blocks** with file titles, numbered match/context rows, explicit match identity, file-derived highlighting, and bounded groups. |
| `bash` / hidden `shell` | Inline `$`, `Writing command...` | **Structured command/output block** with a highlighted `$ <command>` line, plain ANSI-stripped output, textual exit/timeout/truncation status, and bounded collapse. The hidden alias has no separate renderer. |
| `glob` | Inline `✱` + pattern/path/count, `Finding files...` | Inline summary with count/truncation metadata. |
| `task` | `│` / `Delegating...` | Per-member `TaskMemberRow` with `✓` / `✗` / `│`; clicking opens observation. |
| `apply_patch` | Inline or diff-style pending | Existing per-file semantic diff blocks. |
| `todowrite` | Inline `⚙`, `Updating todos...` | Block `# Todos` when todos are present. |
| `question` | Inline `→`, `Asking questions...` | Block `# Questions` when answers are present. |
| `webfetch` / `websearch` / `skill` | Existing inline status | Existing inline result summary. |
| generic | Tool name | Inline or optional block controlled by generic-output preference. |

**Command block** (completed `bash` or stale hidden `shell` request):

```text
<title>
$ <command>
[cwd <cwd>]
<plain output>
<exit / timeout / truncation status>
```

The title comes from the bounded result envelope; `cwd` is a separate
allowlisted row when present. Neither is derived from `env`. A nullable exit is
valid for timeout/signal termination and does not hide captured output. Only
the command line is syntax-highlighted. ANSI control sequences are stripped
from plain output before display, output is collapsed to a bounded preview, and
click-to-expand is local reversible state. A truncated result displays its
status/artifact metadata, not an unbounded capture.

**Diagnostics:** validate every bounded entry, then keep only the first three
severity-1 entries that have a `range.start`; display each as one-based
`Error [line:col] message` under final written/edited paths. Malformed entries
select the existing safe fallback rather than being skipped. The displayed text
and line ranges come from final post-formatter bytes, so formatter changes
cannot make a Read, Write, or Edit preview lie.

### Inline diff rendering

Syntax-highlighted with line numbers and final-state metadata. At 80 columns
the coding-tool diff uses unified layout so the prompt and footer remain usable.
At widths above 120 columns it may use split layout; `diff_style: stacked`
forces unified layout at every width. Wrapping is controlled by the
`diff_wrap_mode` KV flag (`word` | `none`, toggled by `app.toggle.diffwrap`).
Per-file titles include `# Deleted`, `# Created`, `# Moved a → b`, and
`← Patched`. Read/Write text blocks use the same width-aware clipping and
bounded expand/collapse rules.

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
| **Agent models** | `agent.model.list`, `/agent-models` | Lists every catalog Agent, including subagents and hidden system Agents. Configured direct/category rows remain visible but disabled; selecting another row reuses the existing model picker and persists immediately through the backend. |
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
| **Export options** | `session.export`, `<leader>x`, `/export` | Filename default `session-<id8>.md`; Space-toggled switches for thinking, tool details, assistant metadata, **open without saving**; Tab between fields. Always calls `$VISUAL`/`$EDITOR` when set (see [External editor](#external-editor)). |
| **File picker** | Autocomplete / DialogTag | File attach picker |
| **Confirm / Alert / Prompt** | Various | Reusable `DialogConfirm`, `DialogAlert` (retry-error text), `DialogPrompt` |

### Session quick slots

Up to nine pinned **root** sessions are persisted and exposed as ordered quick
slots via `session.quick_switch.1`…`9` (`<leader>1`…`<leader>9`), registered on
the always-available `app.global` layer so they work from any pane. Pin or unpin
from the Sessions dialog with `session.pin.toggle` (`ctrl+f`). Stale pins are
filtered on read.

### Agent model preferences

`/agent-models` first selects a target Agent, grouped as **Main**, **Subagent**,
and **System**, then opens the existing model picker with a title such as
**Select model for compaction**. The target list is separate from `/agents`:
`/agents` and Tab cycling remain limited to root-selectable primary Agents,
while Agent model configuration includes primary, subagent, and hidden internal
rows.

Each row shows its effective `provider/model` and source: `configured`,
`remembered`, or `default`. A retained model that is no longer in the current
provider catalog is labeled **stale preference** and is not used. An Agent with
a direct model or category policy is visible but disabled because configuration
has higher precedence.

A successful selection writes the base provider/model identity immediately to
the backend's active Session database; clean TUI shutdown is not required.
Attached and remote TUIs update that backend, not a local preference file.
Reasoning variants, Session hydration, CLI overrides, and Workflow Stage routes
remain request state and are not stored as Agent preferences. Normal `/models`
selection also remembers the current primary Agent when that Agent is settable.

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

**Submit semantics:** while mode is `shell`, submitting the prompt does **not**
send the buffer to the model. It calls `session.shell` with the buffer as
`command` (plus current session, agent, and model), then **auto-resets** the
prompt to `normal` mode. So `!` is an alternate submit path that runs a shell
turn, not a cosmetic re-theme of a normal prompt.

### Prompt autocomplete

| Trigger | Completes | Notes |
| --- | --- | --- |
| `@` | Files, agents, reference aliases, MCP resources | Fuzzy threshold `0.5`; directory drill-down via a trailing `/` |
| `/` at column 0 | Slash commands | Match on title/display and description |

Keys: Up/`ctrl+p` and Down/`ctrl+n` move, Return selects, Tab completes, Escape
hides. Prompt history is disabled while autocomplete is open.

### Attachments, paste, history, stash

- **`prompt.paste` (`ctrl+v`):** inserts clipboard text or attaches a clipboard image as a `clipboard` file part. Large pasted text collapses into a summarized virtual extmark that expands on copy-out or in the external editor, controlled by `app.toggle.paste_summary` / KV `paste_summary_enabled` (seeded from `experimental.disable_paste_summary`).
- **Local path / drag-and-drop paste:** if the pasted text looks like a local path or `file://` URL (quotes stripped; on non-`win32`, shell-style backslash escapes unescaped), the prompt tries to attach that file before treating the paste as plain text. `http(s)://` URLs are never fetched. Only these extensions attach: `.avif`, `.gif`, `.jpeg`, `.jpg`, `.pdf`, `.png`, `.svg`, `.webp`. Other paths (including `.txt` / `.md` / source files) paste as literal text. `.svg` is special-cased as **inline text** behind an `[SVG: <name>]` placeholder; the rest of the set attach as binary media parts.
- **History:** Up at buffer start walks back; Down at buffer end walks forward; restores text, parts, and shell/normal mode.
- **Stash:** `prompt.stash` saves and clears; `prompt.stash.pop` restores the newest entry; `prompt.stash.list` opens the dialog. A non-empty prompt is auto-stashed across prompt remounts.

### Automatic agent switching

- Switching to a session adopts the agent and model of its last user message when that agent is primary, unless `--agent` was passed on the CLI.
- A completed **`plan_exit`** tool part (status `completed`) switches the local
  agent to `build`. Switches are deduped by part id.
- The TUI also contains a dead branch for a **`plan_enter`** tool name that would
  switch to `plan`. **No `plan_enter` tool is registered** in `hya-tool` (only
  `plan_exit` / alias `plan` exist), so that branch never runs for a real model
  tool call.

### External editor

Shared helper: `openEditor` suspends the TUI, opens `$VISUAL` or `$EDITOR` on a
temp `.md` file (or export content), and resumes the renderer. No editor set →
no-op. Non-zero exit → `Editor exited with code/signal …`.

**`prompt.editor`** (`<leader>e`, `/editor`): edits the current prompt buffer in
the project worktree/directory, then re-imports the edited text and re-anchors
file/agent extmarks, dropping parts whose virtual text was deleted.

**`session.export`** (`<leader>x`, `/export`): after building the transcript
markdown, **always** opens the same editor when `$VISUAL`/`$EDITOR` is set:

| Dialog option | Behaviour |
| --- | --- |
| **Open without saving** | Opens the transcript in the editor only; no export file is written. |
| **Save to file** (default) | Writes `session-<id8>.md` (or the chosen name), opens the editor on that content, then **overwrites the file with the editor’s returned text** if the editor exits successfully. A success toast reports the filename. |

Users who only want a file on disk without an interactive editor session need an
environment without `$VISUAL`/`$EDITOR`, or should expect the editor to open.

---

## Chrome and system integration

### Copy and clipboard

Behavior is gated by `HyaFlag.disableCopyOnSelect`
(`process.platform === "win32"` **or** truthy `HYA_DISABLE_COPY_ON_SELECT`).
Despite the name, the flag switches between **auto copy-on-select** and
**explicit-copy** modes — it does not turn all copying off.

| Mode | When | Mouse-up | Right-click | Ctrl+C over selection | Escape |
| --- | --- | --- | --- | --- | --- |
| Auto copy-on-select | Default on non-`win32` when the env is unset | Copies selection | Off | Off (normal keybinds) | Off (normal keybinds) |
| Explicit copy | `win32`, or `HYA_DISABLE_COPY_ON_SELECT` set | Off | Copies selection | Copies instead of exit | Clears selection |

- The OpenTUI console binds Ctrl+Y to copy-selection (independent of this flag).

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
nested `theme` object key. Files that parse as JSON but fail the theme shape
check (`isTheme`) are skipped for that name only. **Malformed JSON is different:**
`discoverThemes` has no try/catch around `JSON.parse`, so one bad `.json` file
rejects the whole discovery promise; `syncCustomThemes` then catches and force-
resets the active theme to `hya` without installing any custom themes.

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
