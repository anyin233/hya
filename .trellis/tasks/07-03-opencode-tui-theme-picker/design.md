# TUI theme picker parity slice design

## Scope

Branch: `feat/opencode-tui-theme-picker`

Worktree: `.worktrees/opencode-tui-theme-picker`

Assigned version: `0.29.5`

Primary files:

- `crates/hya-legacy-tui/src/lib.rs`
- `crates/hya-legacy-tui/src/theme.rs`
- `crates/hya-backend/src/tui/commands.rs`
- `crates/hya-backend/src/tui/controller.rs`
- `crates/hya-legacy-tui/tests/tui_render.rs`
- Release metadata files required by AGENTS.

## Design

Expose a small theme catalog from `hya-legacy-tui`:

- `ThemeId` enum or string ids.
- `Theme::by_id(id)` with fallback to `hya_dark`.
- `available_themes()` returning labels/details for controller dialogs.

Add `AppState.theme` with a default of the existing dark theme. `draw` resolves the theme from state and passes the same semantic `Theme` to existing widgets.

Add `CommandKind::Themes`, slash aliases `/theme` and `/themes`, and `DialogMode::Theme`. The controller opens a dialog and selection mutates `app.theme` without returning a backend effect.

## Non-goals

- No persistent user config.
- No plugin-provided theme loading.
- No broad visual redesign or changes to widget layout.
