# Implementation plan: TUI borderless opencode-parity redesign

## Prerequisites

- Parent task: `06-21-tui-parity-followup`
- Decision-complete: PRD and design.md approved.
- No engine/proto/provider API changes.

## Rollback point

The entire change is contained in `crates/yaca-tui/` and `crates/yaca-cli/src/tui.rs`/
`main.rs`. If anything fails final validation, revert those files and remove the
new example; no migrations needed.

## Ordered checklist

### 1. Add dependencies

- Edit `Cargo.toml` (workspace root):
  - Add `unicode-width = "0.1"` and `unicode-segmentation = "1.11"` to
    `[workspace.dependencies]`.
- Edit `crates/yaca-tui/Cargo.toml`:
  - Add `unicode-width = { workspace = true }` and
    `unicode-segmentation = { workspace = true }` under `[dependencies]`.

**Validation:** `cargo check -p yaca-tui` compiles (will fail on missing imports
until step 3; that's OK).

### 2. Theme struct and default palette

- Create `crates/yaca-tui/src/theme.rs`:
  - `Theme` struct with fields from design.md.
  - `Theme::opencode_dark()` constructor returning the palette from
    opencode's `opencode.json`.
- Add `pub mod theme;` and `pub use theme::Theme;` in `crates/yaca-tui/src/lib.rs`.
- Add `pub theme: Theme` to `AppState` (default to `Theme::opencode_dark()`).

**Validation:** `cargo check -p yaca-tui` passes.

### 3. InputState and unit tests

- Create `crates/yaca-tui/src/input.rs`:
  - `InputState` struct and all methods listed in design.md §5.3.
  - Grapheme operations via `unicode_segmentation::UnicodeSegmentation`.
  - Width via `unicode_width::UnicodeWidthStr`.
  - Helper: `graphemes(s: &str) -> Vec<&str>`.
- Add `pub mod input;` and `pub use input::InputState;` in `lib.rs`.
- Create `crates/yaca-tui/tests/input_state.rs`:
  - ASCII motion/deletion tests.
  - CJK width tests (`cursor_column`, `visible_slice`, backspace).
  - Word-boundary tests for punctuation and whitespace.

**Validation:** `cargo test -p yaca-tui input_state` passes.

### 4. Redesign main layout

- Rewrite `draw()` in `crates/yaca-tui/src/lib.rs`:
  - Remove `Borders::ALL` from transcript and input.
  - Implement status bar (1 row, `background_panel`).
  - Implement transcript area (`background`, padding 1).
  - Implement input area (3 rows, `background_element`).
  - Implement footer (1 row, `background_panel`).
  - Keep overlay functions but use theme colors.
- Ensure the wrapped-line scroll math accounts for the new inner width.

**Validation:** Existing `tui_render.rs` compiles (assertions will fail until
step 10; that's OK).

### 5. Move model name into input area

- Remove model display from `status_line()`.
- Render model label inside the input area (left side) as a muted span.
- Keep `AppState.model` for engine/agent updates.

**Validation:** Visual inspection via `cargo run --example preview -p yaca-tui`.

### 6. Wire InputState into AppState and event loop

- Change `AppState.input` from `String` to `InputState`.
- Update `yaca-cli/src/tui.rs`:
  - `handle_key` routes keys to `InputState` methods per design.md §5.4.
  - `Action::Submit` uses `app.input.text()` instead of `app.input`.
  - After submit, `app.input.clear()`.
  - History navigation (Up/Down) uses a simple `Vec<String>` ring in `AppState`
    (push on submit, recall into `InputState`).
  - Cursor positioning calls `app.input.cursor_column()` and
    `app.input.visible_slice(width)`.

**Validation:** `cargo check -p yaca-cli` passes.

### 7. Update overlay styling

- In `draw_permission`, `draw_question`, `draw_picker`:
  - Set block background to `theme.background_panel`.
  - Use `Borders::ALL` with `border_style(theme.border)` (subtle border for
    floating surfaces is intentional).
  - Use theme colors for titles/options.

**Validation:** `cargo run --example preview -p yaca-tui` shows styled overlays.

### 8. Add `--mock` CLI flag

- Edit `crates/yaca-cli/src/main.rs`:
  - Add `mock: bool` to `Cli`.
  - Update `cmd_tui` signature to accept `mock: bool` and pass `cli.mock`
    when calling it.
  - In `cmd_tui`, build a `RuntimeConfig` from `offline_router(model_override)`
    when `mock` is true; otherwise use `resolve_runtime(model_override)`:
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
    Do **not** change `offline_router`'s return type; other callers rely on it.

**Validation:** `cargo run --bin yaca -- --mock` launches and echoes typed text.

### 9. Add render-preview example

- Create `crates/yaca-tui/examples/preview.rs`:
  - Build 4-5 fixtures (empty, chat, tool, overlay, CJK).
  - Render each to `TestBackend` and print the buffer.
- Add `[[example]]` entry in `crates/yaca-tui/Cargo.toml` if needed (Cargo
  auto-discovers `examples/*.rs`).

**Validation:** `cargo run --example preview -p yaca-tui` exits 0 and prints
layout.

### 10. Update existing render tests

- Edit `crates/yaca-tui/tests/tui_render.rs`:
  - Replace assertions that look for `"conversation"`/`"message"` titles.
  - Assert model name appears in the input area.
  - Assert status bar no longer contains the model name (or update if kept).
  - Add a test for CJK input rendering.

**Validation:** `cargo test -p yaca-tui` passes.

### 11. Final quality gate

Run the workspace-level gates:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Fix any warnings/errors before declaring done.

## Follow-up checks after implementation

- [ ] Manually run `cargo run --example preview -p yaca-tui` and verify the
      borderless color blocks.
- [ ] Manually run `cargo run --bin yaca -- --mock`, type a message, press
      Enter, and confirm the echo response streams in.
- [ ] Verify CJK input in the mock TUI (cursor position, backspace, delete).
- [ ] Confirm `cargo clippy --workspace --all-targets -- -D warnings` is clean.
