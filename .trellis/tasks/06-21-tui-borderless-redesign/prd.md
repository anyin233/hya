# TUI borderless compat-parity redesign

## Goal

Redesign the yaca interactive TUI so it looks and feels like compat's TUI:
a borderless layout where regions are distinguished by background color blocks
(not box borders), the current model name shown inside the input area, and a
fully capable text input that supports the complete editing keybinding set
(including emacs-style bindings). Add a mock backend so the TUI can be iterated
quickly without real models or network.

User value: a polished, modern chat surface that matches the compat experience,
plus a fast inner-loop for visual/interaction iteration.

## Confirmed facts (from inspection)

- **Current render is fully bordered.** `yaca-tui::draw` (crates/yaca-tui/src/lib.rs)
  wraps the transcript, input, and every overlay in `Block::borders(ALL)`; the
  status line sits on its own top row. This is the look we are replacing.
- **Current input is minimal.** `handle_key` (crates/yaca-cli/src/tui.rs) only
  supports append / Backspace(pop) / Enter. `AppState.input` is a plain `String`
  with **no cursor position**; Left/Right are bound to scroll; there are **zero**
  emacs bindings. Model name is rendered in the top status line, not the input.
- **A render/mock harness already exists.** `crates/yaca-tui/tests/tui_render.rs`
  drives `AppState` -> `draw` on a ratatui `TestBackend` and asserts on the
  rendered buffer (snapshot-style). Good seed for fast visual iteration.
- **A fake backend already exists.** `yaca-provider` ships `FakeProvider`
  (fake.rs / dev.rs) used as the offline echo provider, so a full-loop mock does
  not start from zero.
- **Dependency posture.** Workspace pins deps in `[workspace.dependencies]`;
  ratatui 0.28 + crossterm 0.28. There is currently **no** `tui-textarea`,
  `unicode-width`, or `unicode-segmentation`. Any new dep is a deliberate add.
- **CJK matters.** The user composes in Chinese, so the editor must handle
  wide/double-width glyphs and grapheme clusters correctly (cursor column math,
  delete-by-grapheme, horizontal scroll).
- **compat reference.** The upstream TUI is TypeScript + SolidJS + the custom
  `@opentui/core` renderer (NOT Ink or Go). Key reference files:
  - `packages/tui/src/routes/session/index.tsx` — session layout: row with main
    content (`flexGrow`, `paddingLeft/Right={2}`, `paddingBottom={1}`) + sidebar
    (`width={42}`, `backgroundColor={theme.backgroundPanel}`).
  - `packages/tui/src/component/prompt/index.tsx` — prompt/textarea component;
    model/provider context is rendered adjacent to the prompt area.
  - `packages/tui/src/theme/assets/opencode.json` — default dark palette
    (background #0a0a0a, backgroundPanel #141414, backgroundElement #1e1e1e,
    primary #fab283, text #eeeeee, textMuted #808080, etc.).
  - `packages/tui/src/config/keybind.ts` — canonical keybinding definitions.
    Input defaults include emacs motion/deletion, word movement, selection,
    undo/redo, newline, submit, and history.

## Requirements

1. **Borderless color-block layout (1:1 with compat).** Replace all
   `Borders::ALL` chrome with an compat-style borderless layout where regions
   (transcript, input, status bar, overlays) are separated by background
   color blocks / padding. Match compat's structure and color roles.
2. **Mock backend for fast TUI iteration.** Provide BOTH:
   (a) a **render-preview harness** (example binary) that paints canned
   `AppState` fixtures for instant visual iteration with no async/engine; and
   (b) a **`yaca --mock` live loop** that wires the existing `DevProvider` into
   the real TEA event loop so full interaction (streaming, tools, overlays,
   input) runs offline with no model/network access.
3. **Model name in the input box.** Surface the active model name inside the
   input area (compat shows it in the editor/prompt context), not only the
   top bar.
4. **Full input editing support.** The input must accept the complete set of TUI
   editing operations a user expects, including emacs-style bindings
   (Ctrl-A/E/W/K/U, Alt-B/F, Ctrl-B/F/D, Home/End, word/line motion), correct
   cursor rendering, and correct CJK/grapheme handling. Reference compat's
   editor/textarea behavior.

## Acceptance Criteria

- [ ] No `Borders::ALL` box chrome remains in the main chat layout; regions are
      visually separated by color blocks matching compat's design.
- [ ] The active model name is visible inside the input area.
- [ ] The input editor supports the agreed full keybinding set (enumerated in
      design.md), verified by unit tests over cursor/edit operations.
- [ ] CJK input edits correctly (cursor column, backspace-by-grapheme, no panics
      on wide chars), verified by a test with multi-byte/double-width input.
- [ ] The render-preview example runs and produces the same fixture output
      deterministically (suitable for snapshot-style iteration).
- [ ] `yaca --mock` launches the TUI and runs turns against the offline
      `DevProvider` without requiring config or network.
- [ ] `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
      and `cargo test --workspace` all pass.

## Out of scope

- The 7 backlog deliverables in the parent task (Stash, Status, Tags, Variants,
  MCP, Warp, Console/share) unless directly entailed by the redesign.
- Provider OAuth / in-TUI provider-connect wizard (excluded by direction).
- Syntax highlighting (syntect) and full-screen DiffViewer polish.
- Multi-line expanding input. This task implements a single-line chat input with
  full emacs-style motion/deletion; multi-line text entry is a future
  enhancement.

## Open questions

Resolved:
1. Mock backend shape: both preview harness + `--mock` live loop.
2. Editor implementation: custom widget using `unicode-width` +
   `unicode-segmentation`; no `tui-textarea` dependency.
3. Theme palette: port compat's default dark theme.
4. Input scope: single-line chat input with full emacs-style editing (motion,
   deletion, word/line navigation); multi-line expansion deferred.
