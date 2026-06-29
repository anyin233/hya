# Design: TUI borderless opencode-parity redesign

## 1. Architecture & boundaries

The change is confined to the view crate `hya-render-tui` and the CLI event-loop
`hya-cli/src/tui.rs`. No engine, provider, store, or proto APIs change.

- `hya-render-tui` stays a **pure view**: it owns `AppState`, `Theme`, a new
  `InputState`, and the `draw` function.
- `hya-cli/src/tui.rs` stays the **event loop**: it translates crossterm keys
  into `InputState` method calls and folds engine events into `AppState`.
- `hya-provider` already exposes `DevProvider`; the mock "backend" is mostly
  wiring, not new provider code.

## 2. Reference summary (opencode)

opencode's TUI (`sst/opencode`, `dev` branch, `packages/tui/src`) is built on a
custom TypeScript/SolidJS renderer (`@opentui/core`). The design we port is:

- **Layout**: a top-level `flexDirection="row"`. Main content is a column with
  `paddingLeft={2} paddingRight={2} paddingBottom={1} gap={1}`; sidebar is a
  fixed `width={42}` panel with `backgroundColor={theme.backgroundPanel}`.
- **Chrome**: borders are not used as primary separators; `EmptyBorder` is the
  default. Regions are separated by `backgroundColor` props (`background`,
  `backgroundPanel`, `backgroundElement`) and padding.
- **Palette** (default dark `opencode.json`):
  - `background` `#0a0a0a`
  - `backgroundPanel` `#141414`
  - `backgroundElement` `#1e1e1e`
  - `borderSubtle` `#3c3c3c`, `border` `#484848`, `borderActive` `#606060`
  - `text` `#eeeeee`, `textMuted` `#808080`
  - `primary` `#fab283`, `secondary` `#5c9cf5`, `accent` `#9d7cd8`
  - `success` `#7fd88f`, `warning` `#f5a742`, `error` `#e06c75`, `info` `#56b6c2`
- **Editor**: a managed `TextareaRenderable` driven by `@opentui/keymap`. The
  default binding map includes char/word/line/buffer motion, selection,
  deletion, undo/redo, newline, and submit.

## 3. Theme system

Introduce a small `Theme` struct in `hya-render-tui` (no file IO; the default dark
palette is compiled in). Use ratatui `Color::Rgb` values from the opencode
palette.

```rust
pub struct Theme {
    pub background: Color,
    pub background_panel: Color,
    pub background_element: Color,
    pub border: Color,
    pub border_active: Color,
    pub border_subtle: Color,
    pub text: Color,
    pub text_muted: Color,
    pub primary: Color,
    pub secondary: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
}
```

Expose `Theme::opencode_dark()` and store it in `AppState` so tests can pass a
deterministic theme.

## 4. Layout redesign (borderless)

Replace the current 3-row vertical layout with a color-block layout:

| Region | Height | Background | Content |
|---|---|---|---|
| Status bar | 1 | `background_panel` | Brand + session label + running/idle pill + yolo/reasoning/goal/loop pills. Model name **removed** from here. |
| Transcript | fill | `background` | Scrollable message history, left/right padding of 1, no border. |
| Input | 3 | `background_element` | Left: model label + prompt chevron; middle: typed text; right: hint text. |
| Footer | 1 | `background_panel` | Hints: "Enter send · Esc quit · ↑↓ history · PgUp/PgDn scroll". |

- All `Block::borders(Borders::ALL)` on the main regions are removed.
- Overlays (permission/question/picker) keep a subtle 1-line border using
  `border`/`border_active` colors because they are floating modal surfaces;
  this is intentional and matches opencode's dialog styling.
- The transcript stays scrollable with the existing scroll-back math, but the
  wrapped-line calculation must account for the new padding.

## 5. Input editor

### 5.1 Scope

Single-line chat input with full emacs-style motion and deletion. Multi-line
expansion, selection, and undo/redo are **deferred** (documented in prd.md out
of scope).

### 5.2 State model

Replace `AppState.input: String` with `AppState.input: InputState`.

```rust
pub struct InputState {
    text: String,
    cursor: usize,        // grapheme index, 0..=grapheme_count
    scroll_cols: usize,   // display columns hidden on the left
}
```

- All edits operate on **grapheme clusters** via `unicode-segmentation`.
- Display width uses `unicode-width` so CJK characters count as 2 columns.
- `scroll_cols` keeps the cursor visible: after any edit or motion, ensure
  `cursor_col >= scroll_cols` and `cursor_col < scroll_cols + visible_width`.

### 5.3 Operations

```rust
impl InputState {
    pub fn move_left(&mut self);
    pub fn move_right(&mut self);
    pub fn move_word_left(&mut self);
    pub fn move_word_right(&mut self);
    pub fn move_home(&mut self);
    pub fn move_end(&mut self);
    pub fn backspace(&mut self);
    pub fn delete(&mut self);
    pub fn delete_word_backward(&mut self);
    pub fn delete_word_forward(&mut self);
    pub fn delete_to_start(&mut self);
    pub fn delete_to_end(&mut self);
    pub fn insert(&mut self, ch: char);
    pub fn insert_str(&mut self, s: &str);
    pub fn clear(&mut self);
    pub fn set_text(&mut self, s: &str);
    pub fn text(&self) -> &str;
    pub fn is_empty(&self) -> bool;
    pub fn cursor_column(&self) -> usize;       // screen column of cursor
    pub fn visible_slice(&self, max_width: usize) -> &str; // grapheme-aligned slice
}
```

### 5.4 Keybindings (implemented)

| Action | Keys |
|---|---|
| Move left | `Left`, `Ctrl+B` |
| Move right | `Right`, `Ctrl+F` |
| Word backward | `Alt+B`, `Ctrl+Left` |
| Word forward | `Alt+F`, `Ctrl+Right` |
| Start of input | `Ctrl+A`, `Home` |
| End of input | `Ctrl+E`, `End` |
| Backspace | `Backspace` |
| Delete forward | `Delete`, `Ctrl+D` |
| Delete word backward | `Ctrl+W`, `Alt+Backspace`, `Ctrl+Backspace` |
| Delete word forward | `Alt+D`, `Alt+Delete`, `Ctrl+Delete` |
| Delete to start | `Ctrl+U` |
| Delete to end | `Ctrl+K` |
| Submit | `Enter` |
| History previous | `Up` |
| History next | `Down` |

**Behavior changes from current TUI:**
- `Up`/`Down` no longer scroll the transcript; they navigate prompt history.
- `PgUp`/`PgDn` remain transcript page scroll.
- `Esc` still quits (unchanged).
- `Ctrl+C`/`Ctrl+D` still quit (unchanged).

### 5.5 Rendering

The input area is rendered as a single `Paragraph` with inline spans:

```
[model-label muted] [chevron primary] [typed text]          [hint muted]
```

- Model label is e.g. `claude-sonnet-4-6` truncated to ~20 cols.
- Typed text is horizontally scrolled if it exceeds the available width.
- Cursor is placed with `frame.set_cursor_position` using the visible column
  (prefix width + cursor column - scroll_cols).

## 6. Mock backend

### 6.1 Render preview harness

Add `crates/hya-render-tui/examples/preview.rs`. It constructs several `AppState`
fixtures (empty, short chat, long message, tool call, permission overlay,
picker, yolo mode, CJK text) and renders each to a `TestBackend`. Output is
written to stdout so a developer can run:

```sh
cargo run --example preview -p hya-render-tui
```

and inspect the layout without launching the full engine.

### 6.2 `--mock` live loop

Add a `--mock` global flag to `hya-cli/src/main.rs`:

```rust
#[arg(long, global = true)]
mock: bool,
```

When `--mock` is set, `cmd_tui` bypasses config resolution and uses the offline
router. Because `offline_router` returns `(ProviderRouter, String)` while
`resolve_runtime` returns `RuntimeConfig`, the mock branch builds the equivalent
`RuntimeConfig` inline without changing `offline_router`'s existing callers:

```rust
let runtime = if mock {
    let (router, model) = offline_router(model_override);
    RuntimeConfig {
        router,
        model,
        models: Vec::new(),
        mcp: BTreeMap::new(),
    }
} else {
    resolve_runtime(model_override)
};
```

This forces `DevProvider` (echoes the user's prompt back) so the TUI event loop,
streaming, overlays, and input editor can be exercised without API keys or
network. `cmd_tui` gains a `mock: bool` parameter; `Cli::mock` is passed through.

## 7. Dialog/overlay styling

Keep the existing overlay shapes but update their `Block` styling:

- Use `Border::NONE` or a thin border colored with `theme.border`.
- Background set to `theme.background_panel`.
- Title color `theme.text` or `theme.primary` depending on overlay type.
- Selected option background `theme.primary` with selected-foreground black
  (opencode uses `selectedListItemText: background`).

## 8. Compatibility & migration

- This is a visual-only change. Saved sessions, config files, and the CLI
  surface outside `--mock` are unaffected.
- Existing tests in `crates/hya-render-tui/tests/tui_render.rs` assert on border
  titles (`"conversation"`, `"message"`, `"permission required"`, etc.). These
  assertions are updated to match the new layout.

## 9. Testing strategy

| Area | Tests |
|---|---|
| Editor | Unit tests in `crates/hya-render-tui/tests/input_state.rs` for every operation: motion, word deletion, CJK width, visible slice/scroll, home/end. |
| Layout | Update `tui_render.rs` assertions: no `"conversation"` title, model inside input, status bar contents, footer hints. |
| Theme | Snapshot-style `TestBackend` test verifying background colors of status/input/footer regions. |
| Mock | `hya --mock` integration smoke test (manual or script): launch, type, verify echo response. |
| Regression | `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`. |

## 10. Risks & rollback

- **CJK width bugs**: mitigated by dedicated unit tests and using the standard
  `unicode-width` crate.
- **Cursor drift**: mitigated by computing cursor column from grapheme widths
  and clamping to the visible window after every operation.
- **Broken existing tests**: expected; update assertions in the same PR.
- **Rollback**: revert the two crates (`hya-render-tui`, `hya-cli`) to the previous
  commit; no data or schema migrations are involved.

## Plan Review

### Round 1 — 12th/claude-opus-4-7 — VERDICT: FAIL

D1 PASS
D2 PASS
D3 FAIL: design.md:203-209 snippet `if mock { offline_router(...) } else { resolve_runtime(...) }` is type-incompatible — `offline_router` returns `(ProviderRouter, String)` at main.rs:295-301, but `resolve_runtime` returns the `RuntimeConfig` struct at main.rs:303-355 (consumed via `runtime.router` / `runtime.model` / `runtime.models` / `runtime.mcp` at main.rs:534-557); the plan never says whether to refactor `offline_router` or wrap its tuple, so step 8 won't compile as written -> add an explicit substep to build a `RuntimeConfig` from `offline_router` so both branches yield the same type [implement.md:101-109]
D4 PASS
D5 PASS
D6 PASS
VERDICT: FAIL

Fix applied: design.md §6.2 and implement.md §8 now show building a `RuntimeConfig`
inline from `offline_router` without changing its signature, and `cmd_tui` gains
an explicit `mock: bool` parameter.

### Round 2 — 12th/claude-opus-4-7 — VERDICT: PASS

D1 PASS
D2 PASS
D3 PASS: Round 1 type mismatch is fixed — implement.md:107-122 and design.md:204-217 now build `RuntimeConfig { router, model, models: Vec::new(), mcp: BTreeMap::new() }` from `offline_router`'s `(ProviderRouter, String)` tuple, matching the struct at main.rs:303-308 verbatim; `offline_router`'s signature at main.rs:295-301 is preserved so the other five callers keep compiling; `BTreeMap` is already imported at main.rs:17.
D4 PASS
D5 PASS
D6 PASS
VERDICT: PASS
