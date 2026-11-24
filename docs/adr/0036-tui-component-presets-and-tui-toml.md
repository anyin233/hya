# ADR-0036: TUI component presets and `tui.toml`

Date: 2026-11-23
Status: Accepted

## Context

The TUI (ADR-0019) draws one fixed arrangement: the PaneLayout v4 tree with
Projects on the left, the conversation viewer, an activity line, and the
message editor in the middle, and Sessions, Todos, and Context on the right
(docs/tui.md "Tiled workspace"). "Conversation without headings" removed every
metadata row from the conversation, so the model, the permission mode, and the
context occupancy appear only in the right sidebar, which needs 150 columns.
At 80 to 149 columns, a common width for a terminal or a WebUI tab, the screen
shows a transcript and a boxed input and nothing about the session.

Users want different amounts of structure: a bare transcript, a transcript
with a one-row status line, an omp-style status band with a dock of todos and
subagents, or the full tiled workspace. The current code expresses each of
these as edits to a saved layout tree, and none of them is the default.

`tui.json` holds state that TUI commands write: the theme, vim, keybindings,
the saved `paneLayout`, and extension trust. It has no place for a structure
the user writes once and keeps.

## Decision

### Decisions

- **D1** The default structure is `statusline` (structure B): the transcript,
  the input, and one status row below the input that names the model, the
  permission mode, and the context occupancy at any width.
- **D2** Structure is configured in a declarative `tui.toml`. The TUI only
  reads it and never runs code from it: no scripts, file references, or
  component registration. Custom rendering stays with TUI extension bundles.
- **D3** Four built-in presets: `minimal` (A), `statusline` (B, default),
  `dock` (C), and `workspace` (D). `tui.toml` may select one, override single
  components on top of it, or define named presets that `extends` another
  (at most 8 levels, no cycles).
- **D4** No component is removed. The sidebars, the Todos and Context panes,
  the layout tree, the layout editor, and drag resizing stay and make up the
  `workspace` preset.
- **D5** Precedence, later wins: built-in preset, named preset chain, the
  top-level component tables of `tui.toml`, then `/set` for the current
  session. Tables merge key by key; arrays replace. `enabled = false` turns a
  component off and keeps its other settings.
- **D6** Configuration errors never block startup. A missing file is silent;
  a syntax error ignores the file; an unknown key, a wrong type, or an unknown
  name ignores that item; an unknown preset or a bad `extends` chain falls
  back to the default. Every warning is listed by `/config`.
- **D7** `tui.toml` and `tui.json` share no keys. `tui.toml` is user-written
  structure; `tui.json` stays command-written state. Only `/config init`
  creates `tui.toml`, and only when it does not exist.
- **D8** There is no project-level `.hya/tui.toml`. The file belongs to the
  machine that runs the TUI, including in the WebUI (the PTY host) and with a
  remote backend.
- **D9** The theme and keybindings are not part of `tui.toml` v1. A v1 loader
  warns that `[theme]` and `[keys]` are not supported yet. The reserved
  contract is in docs/tui-config.md "Reserved tables" and needs its own
  change.
- **D10** One layout engine draws every preset. The single-column presets
  place a fixed chat column (conversation, activity, editor) in the existing
  pane engine, so keyboard ownership, paste routing, and the dock heights keep
  one implementation. The `workspace` preset uses the saved `paneLayout`.
  Only the `workspace` preset reads or writes `paneLayout`; `/layout` editing
  commands are refused elsewhere, and the saved tree is never overwritten by a
  single-column preset.

### Deduplication

The same fact is shown in one resident place:

- While a visible sidebar holds Todos, the dock's todo rows collapse.
- While a visible sidebar holds Context, the status line collapses.
- When the width hides a sidebar, the dock and the status line take over its
  facts. `workspace` below 150 columns therefore looks like `statusline`.

### Interface rule update

"A box means it is selectable" (docs/tui.md "Tiled workspace") stays. One
exception is added: attachment cards above the input (p2 P1) are part of the
composer, use their attachment identity color instead of the focus color, and
respond to clicks like the attachment rows they replace.

## Consequences

- This ADR supersedes the "Conversation without headings" rule that no
  metadata row appears next to the conversation, and the statement that the
  seven-pane tiled workspace is the default arrangement. Both now describe the
  `workspace` preset only.
- A user who relied on the sidebars writes `preset = "workspace"` in
  `tui.toml`. A TUI that finds a saved `paneLayout` and no `tui.toml` says so
  once and records that it did.
- Shared terminal specs choose their preset explicitly when they depend on
  sidebars; the harness writes `tui.toml` next to the isolated `tui.json`.
- The status line and the startup list are display only. They read the
  existing projection and catalog contracts and append no events.
- Later phases add `dock`, startup resources, configurable sidebars, and
  PaneLayout v5; each extends the `tui.toml` schema without changing a key.
