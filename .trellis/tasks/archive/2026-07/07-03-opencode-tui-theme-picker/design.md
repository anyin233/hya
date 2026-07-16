# TUI theme picker parity slice design

## Scope

Branch: `feat/opencode-tui-theme-picker`

Base: `feat/opencode-compat-mcp-import`

Assigned version: `0.33.11`

Primary files:

- `crates/hya-tui/src/app/command.rs`
- `crates/hya-tui/src/app/dialog.rs`
- `crates/hya-tui/src/app/harness.rs`
- Release metadata files required by AGENTS.

## Design

Use the existing `hya-tui` theme catalog and `theme.switch` dialog route. Register `/theme` and `/themes` as built-in commands, initialize the selection to the current theme, mark it in the list, and apply a selected theme directly to app state.

## Non-goals

- No persistent user config.
- No plugin-provided theme loading.
- No broad visual redesign or changes to widget layout.
