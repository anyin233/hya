# opencode TUI — consolidated reference inventory

Source: opencode's current TUI, sparse-cloned to `/tmp/opencode-src/packages/tui`
(version 1.17.9). **TypeScript on `@opentui/solid`** (OpenTUI + Solid.js reactive
terminal rendering), ~27k LOC. We port its **appearance + features + keymap** into
**Rust/ratatui** (immediate-mode), keeping **hya** branding.

Full agent dumps (for fine detail / truncated tails) saved at:
- dialogs: `~/.local/share/opencode/tool-output/tool_ee7bae078001dvgGOciKDzTEC4`
- theme/keymap: `~/.local/share/opencode/tool-output/tool_ee7bae1be001BS4pkkMfLWFBfJ`

---

## 1. Architecture (opencode) → proposed hya module mapping

opencode boots a Solid render tree under a deep provider stack; transport is an
SDK + global **SSE** event stream, batched (16ms) into a **normalized store**.

| opencode (TS) | role | hya (Rust/ratatui) target |
|---|---|---|
| `app.tsx` | root: renderer (60fps), provider tree, route switch (home/session), global key dispatch, selection/copy | `AppShell` (event loop in `hya-cli`, state in `hya-render-tui`) |
| `routes/home.tsx` | centered logo + prompt | `HomeView` |
| `routes/session/index.tsx` (2648) | chat transcript, msg/tool/reasoning render, scroll, sidebar mount, prompt-area state machine | `SessionView` |
| `routes/session/sidebar.tsx` | 42-col sidebar (title/workspace/share + slots) | `Sidebar` |
| `routes/session/subagent-footer.tsx` | child-session footer (parent/prev/next + tokens/cost) | `SubagentFooter` |
| `component/prompt/index.tsx` (1697) | extmark editor, modes, submit, paste | `PromptEditor` |
| `component/prompt/autocomplete.tsx` (770) | `@`/`/` completion popup | `Autocomplete` |
| `context/sdk.tsx` + `context/event.ts` | SSE transport + reconnect/backoff | `SseEventPump` (hya-client) |
| `context/sync.tsx` (655) | normalized session/message/part store | `SyncStore` |
| `context/local.tsx` | persisted prefs: agent/model/favorites/pinned sessions | `LocalStore` |
| `theme/index.ts` (1089) | theme schema + 33 built-ins + resolution | `theme` module |
| `component/logo.tsx` (885) + `logo.ts` | splash logo | `Logo` widget |
| `ui/dialog*.tsx` + `component/dialog-*.tsx` | modal stack + ~30 dialogs | `dialog` system |
| `command-palette.tsx` | palette over `namespace:"palette"` cmds | `CommandPalette` |
| `feature-plugins/system/which-key.tsx` | leader keybind hint overlay | `WhichKey` |
| `feature-plugins/system/diff-viewer.tsx` (1059) | split/unified diff + file tree | `DiffViewer` |

Key architectural facts to preserve:
- **Normalized store**: messages keyed by session; parts keyed by message. Assistant
  render = `Message + Part[]` (not nested). 100-message retention; hydration merge
  must not clobber live stream deltas.
- **Prompt-area state machine** (mutually exclusive, bottom of session): pending
  permission → `PermissionPrompt`; else pending question → `QuestionPrompt`; else
  child session → `SubagentFooter`; else normal `Prompt`.
- **Sidebar**: width exactly 42; auto-visible when `termWidth > 120` and root session;
  overlay (dark scrim) on narrow terminals; hidden for child sessions.
- **Scroll**: sticky-bottom scrollbox; page/half/line/first/last/next-prev-message cmds.

---

## 2. Appearance — default theme `opencode` (dark mode, shipped startup look)

From `theme/assets/opencode.json` (+ fallbacks in `theme/index.ts`). Active theme at
startup = `opencode`, mode = `dark`. ratatui should reproduce the **dark** column.

```
token                    dark        token                    dark
primary                  #fab283     markdownText             #eeeeee
secondary                #5c9cf5     markdownHeading           #9d7cd8
accent                   #9d7cd8     markdownLink              #fab283
error                    #e06c75     markdownLinkText          #56b6c2
warning                  #f5a742     markdownCode              #7fd88f
success                  #7fd88f     markdownBlockQuote        #e5c07b
info                     #56b6c2     markdownEmph              #e5c07b
text                     #eeeeee     markdownStrong            #f5a742
textMuted                #808080     markdownHorizontalRule    #808080
selectedListItemText     #0a0a0a     markdownListItem          #fab283
background               #0a0a0a     markdownListEnumeration   #56b6c2
backgroundPanel          #141414     markdownImage             #fab283
backgroundElement        #1e1e1e     markdownImageText         #56b6c2
backgroundMenu           #1e1e1e     markdownCodeBlock         #eeeeee
border                   #484848     syntaxComment             #808080
borderActive             #606060     syntaxKeyword             #9d7cd8
borderSubtle             #3c3c3c     syntaxFunction            #fab283
diffAdded                #4fd6be     syntaxVariable            #e06c75
diffRemoved              #c53b53     syntaxString              #7fd88f
diffContext              #828bb8     syntaxNumber              #f5a742
diffHunkHeader           #828bb8     syntaxType                #e5c07b
diffHighlightAdded       #b8db87     syntaxOperator            #56b6c2
diffHighlightRemoved     #e26a75     syntaxPunctuation         #eeeeee
diffAddedBg              #20303b     diffRemovedBg             #37222c
diffContextBg            #141414     diffLineNumber            #8f8f8f
diffAddedLineNumberBg    #1b2b34     diffRemovedLineNumberBg   #2d1f26
thinkingOpacity          0.6
```
Theme system: tokens support hex / `defs` refs / `{dark,light}` / RGBA / ANSI. 33
built-in themes (aura, ayu, catppuccin*, dracula, github, gruvbox, kanagawa, nord,
one-dark, tokyonight, vesper, vercel, …). Custom themes from `~/.config/opencode/themes/*.json`
and `./.opencode/themes/*.json`. `theme.switch` / `theme.switch_mode` / `theme.mode.lock`.

## 3. Logo (splash) — literal art

`logo.ts` left[] + right[] joined with `GAP=1`; left = `textMuted`, right = `text` bold;
shadow markers `_`=shadow cell, `^`=`▀`, `~`=`▀`, `,`=`▄`. Combined:
```
                                 ▄
█▀▀█ █▀▀█ █▀▀█ █▀▀▄ █▀▀▀ █▀▀█ █▀▀█ █▀▀█
█__█ █__█ █^^^ █__█ █___ █__█ █__█ █^^^
▀▀▀▀ █▀▀▀ ▀▀▀▀ ▀~~▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀
```
Renders centered on home above the prompt (`home_logo` slot). **For hya: keep the
"hya" wordmark** (reuse the same block-glyph style + shadow technique + primary glow).

## 4. Keymap (default; leader = `Ctrl+X`, leader timeout 2000ms)

Aliases: enter→return, esc→escape, pgup/pgdn. `none` = defined but unbound.
Full table in `config/keybind.ts:45-237` (CommandMap 253-413). Highlights:

- App: `ctrl+c`/`ctrl+d`/`<leader>q` app.exit · `ctrl+p` command.palette.show · help.show · docs.open · toggles (animations, file_context, diffwrap, paste_summary)
- Theme/sidebar: `<leader>e` prompt.editor · `<leader>t` theme.switch · `<leader>b` sidebar.toggle · `<leader>s` status
- Session mgmt: `<leader>x` export · `<leader>n` new · `<leader>l` list · `<leader>g` timeline · `ctrl+r` rename · `ctrl+d` delete · `escape` interrupt · `<leader>c` compact · `<leader>1..9` quick-switch · `<leader>down` child.first · `right/left` child.next/prev · `up` parent · `ctrl+f` pin.toggle
- Models/agents: `<leader>m` model.list · `f2`/`shift+f2` model.cycle_recent · `<leader>a` agent.list · `tab`/`shift+tab` agent.cycle · `ctrl+t` variant.cycle · mcp.list · provider.connect
- Messages/viewport: `pageup`/`pagedown` page · `ctrl+alt+u/d` half-page · `ctrl+g`/`home` first · `end` last · `<leader>y` messages.copy · `<leader>u`/`<leader>r` undo/redo · `<leader>h` toggle.conceal · toggle.actions · toggle.thinking
- Editor (managed-textarea layer): `return` input.submit · `shift/ctrl/alt+return`,`ctrl+j` input.newline · readline motions (`ctrl+b/f/a/e`, `alt+f/b`, `ctrl+w`, `ctrl+k/u`), select variants, undo/redo · `ctrl+c` prompt.clear · `ctrl+v` prompt.paste
- Autocomplete: `up/ctrl+p` prev · `down/ctrl+n` next · `escape` hide · `return` select · `tab` complete/expand
- Diff viewer (when open): `escape/q` close · `enter/space` toggle · `]`/`[` next/prev hunk · `n`/`p` next/prev file · `b` file-tree · `v` split/unified · `s` single-patch · `?` help

## 5. Chat / message rendering spec

- **User msg**: colored left rail (agent color), text, file-attachment chips
  (`txt|img|pdf|dir` MIME badge + filename — images are CHIPS, not inline previews),
  `QUEUED` badge or timestamp, optional compaction divider.
- **Assistant msg**: ordered part stream → `text`=markdown block · `reasoning`=collapsible
  ("Thinking…" spinner → "Thought · Ndur"; collapsed 1-line in hide mode) · `tool`=specialized
  renderer. Footer metadata: mode · model · duration · interrupted. Error panel for non-abort failures.
- **Tool renderers** (13+ specialized, else generic): `bash` (title+cmd+ANSI-stripped output, collapsible), `write` (highlighted content + diagnostics), `edit`/`apply_patch` (diff split if width>120, diagnostics), `read` (inline + "Loaded…"), `glob`/`grep`/`webfetch`/`websearch` (compact counts), `todowrite`, `question`, `task` (subagent row, click→child session), `skill`. Completed tools hidden when "show details" off; generic output hidden unless toggled.
- **Markdown**: streaming markdown w/ tables + conceal; syntax highlight via theme `syntax*` tokens.
- **Diff viewer**: split/unified auto (split if width>120), file tree (`b`), hunk nav, wrap toggle; colors from `diff*` tokens.

## 6. Editor spec (component/prompt)

- Multiline wrapped textarea, `minHeight=1`, `maxHeight=max(6, termH/3)`. Visible text ≠
  logical payload: `parts[]` holds file/agent/pasted-text/attachment payloads as virtual
  spans (extmarks) synced to visible chips.
- Modes: normal / **shell** (entered by `!` at col 0 → `session.shell`). Placeholders randomized.
- Submit: `return`; newline: `shift/ctrl/alt+return`,`ctrl+j`. IME-safe (force plain-text sync pre-submit). `exit`/`quit`/`:q` exit app. Slash command parsed from first line → `command + arguments`.
- **Esc interrupt**: 1st arms, 2nd (within 5s) aborts busy session; in shell mode Esc exits shell.
- **`@` completion**: files (`fs.find` limit 20) + reference aliases + non-primary agents + MCP resources + editor mentions; line-range suffix `file#12` / `file#12-30`; frecency-weighted; `Tab` on dir expands `@path/`.
- **`/` completion**: builtin slashes + server commands (skill excluded; mcp `:mcp` suffix); only when prompt starts `/` and no whitespace before cursor.
- Paste: CR→LF normalize; image→attachment; local file path→attachment (svg=text); large paste (≥3 lines or >150 chars) → `[Pasted ~N lines]` placeholder expanded on submit/copy. Attachments `[Image N]`/`[PDF N]`.
- Persistence (JSONL in state dir): history (50, dedupe-prev), stash (50, LIFO), frecency (1000, `freq/(1+ageDays)`).
- External editor `$VISUAL/$EDITOR` (`/editor`), Zed sqlite polling, Claude IDE websocket (`at_mentioned`, `selection_changed`); pending selection injected as `<system-reminder>` on submit.

## 7. Dialog catalog (modal stack: sizes 60/88/116; Esc/Ctrl+C close)

Primitives: `dialog.tsx` (stack/backdrop), `dialog-select.tsx` (searchable list — basis of
most), `dialog-prompt.tsx` (text entry), `dialog-confirm.tsx`, `dialog-alert.tsx`,
`dialog-export-options.tsx`, `dialog-help.tsx`.

Concrete dialogs (★ = needs backend data hya lacks):
- ★`command-palette` (~80 cmds over `namespace:"palette"`)
- ★`dialog-model` (favorites/recents/providers + connect) ← model.list
- ★`dialog-provider` (multi-step connect/OAuth/API-key wizard)
- ★`dialog-session-list` (switch/search/pin/rename/delete/recover) ← session.list
- `dialog-move-session`, `dialog-workspace-create/list/file-changes/unavailable` (warp/move)
- ★`dialog-status` (MCP/LSP/formatters/plugins), ★`dialog-mcp`
- `dialog-retry-action`, `dialog-stash`, `dialog-console-org`, `dialog-session-delete-failed`
- `dialog-agent`, `dialog-skill`, `dialog-tag`, `dialog-session-rename`, ★`dialog-theme-list`, `dialog-variant`
- Inline (not modal): `routes/session/permission.tsx` (allow/allow-always/reject+msg, fullscreen for diffs), `question.tsx` (tabs/multi-select/custom answer/confirm)
- `routes/session/dialog-message`, `dialog-fork-from-timeline`, `dialog-timeline`, `dialog-subagent`
- `which-key.tsx` (leader hint overlay)

## 8. Spinner / toast

- Spinner frames (used): `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` @ 80ms, color `textMuted`; `⋯` when animations off.
- Toast: absolute top-right (margin 2), max width `min(60, screenW-6)`, padding 2x1, left/right border, theme bg, variant colors; auto-dismiss.

---

## 9. hya backend gap → extensions required for parity (decision #2: build them)

Today: 4 routes (`POST /sessions`, `POST /sessions/:id/prompt`, `GET .../events`,
`GET .../stream`); embedded in-process engine; in-memory store (no persistence). To
drive the parity features, hya-core/server/client (+ new `Event`/DTO variants) need:

1. **Session list + metadata + load** (session switcher, quick-switch, continue) — store query + `GET /sessions` + client method.
2. **Session persistence by default** (durable store path, not in-memory).
3. **Model + provider enumeration** (model picker) — expose router/config catalogs + `GET /models`,`/providers`.
4. **Runtime model/agent/mode switch** (pickers, cycle) — engine API + event + endpoint.
5. **Abort/cancel endpoint** (Esc interrupt) — cancel handle per session + `POST /sessions/:id/abort`; honor cancel mid-stream/mid-shell.
6. **Permission round-trip over the wire** (permission prompt) — surface `AskRequest` via events/SSE + decision endpoint; wire the in-proc plane to the TUI.
7. **Question flow** (if adopting opencode question UX) — event + decision endpoint.
8. **Token/cost reporting end-to-end** — engine records usage to store; expose in projection/usage endpoint; status/sidebar/subagent footers.
9. **Session title/rename** — engine `title_session` + `PATCH /sessions/:id/title`.
10. **File find API** (`@` completion) — glob-backed `GET /find` or client helper.
11. **Config: theme + keybind read/write** — config schema + load/save (themes, leader, binds).
12. **Tool schema / command list exposure** (palette, status) — engine getters + endpoints.
13. **Team/goal/loop live status** (if surfacing) — events + subscribe API (snapshots exist; no event/stream).
14. **Reasoning in projection** — currently the projection reducer ignores some lifecycle; ensure reasoning + tool lifecycle fully projected for the richer renderer.

## 10. Proposed wave breakdown (to be finalized in design.md / implement.md)

- **W0 — Baseline + scaffolding**: confirm `cargo build --workspace` green; reconcile the
  in-progress permission refactor; stand up new `hya-render-tui` module structure + theme module
  (port `opencode` dark palette) + a render-test harness (ratatui TestBackend snapshots).
- **W1 — Appearance shell**: AppShell + HomeView (hya logo + theme) + status/footer +
  SessionView transcript with markdown + the existing tool/text/reasoning render upgraded
  to opencode's model. Mouse + sticky-bottom scroll.
- **W2 — Editor parity**: PromptEditor (parts/extmark model, modes, submit/newline, paste,
  history/stash/frecency) + `@`/`/` Autocomplete (needs file-find + command/model list).
- **W3 — Dialog system + core dialogs**: modal stack + command palette + model picker +
  session switcher + theme picker + help/which-key (needs backend: model/session/theme).
- **W4 — Rich rendering**: full specialized tool renderers + split/unified DiffViewer + diagnostics + syntax highlighting.
- **W5 — Flows**: permission + question inline flows (wire round-trip), abort/interrupt, session lifecycle (new/rename/delete/compact/fork/timeline), sidebar.
- **W6 — Backend parity hardening**: persistence, token/cost, providers/connect, MCP/status, agents/modes/variants, team/goal/loop surfacing as applicable.
- **W7 — Fidelity QA**: side-by-side visual QA vs opencode (tmux), keymap parity audit, polish.

Each wave = a Trellis child task with its own prd/acceptance + verifiable in a real terminal.
