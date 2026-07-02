# Ratatui TUI redesign design

## Scope

This design upgrades yaca's TUI presentation while preserving existing behavior and architecture. compat is used only as a visual reference. The implementation remains Rust-only and ratatui-based.

## Current boundary

`crates/yaca-tui` owns pure rendering of `AppState`. `crates/yaca-cli/src/tui.rs` owns terminal setup, crossterm input, async task spawning, bus subscription, and redraw scheduling. This boundary stays unchanged.

## Visual model

The new visual style uses a semantic surface stack instead of a dominant bordered chat panel:

- `background`: terminal base.
- `panel`: user messages, sidebar, prompt background.
- `element`: hover/active-like surface, tool blocks, permission prompt.
- `border_subtle`: quiet separators.
- `border_active`: emphasized lines and focus hints.

User messages become panel blocks with a colored left rail. Assistant text is lighter and indented, so the transcript reads like a timeline rather than alternating full-width cards. Tool calls become compact rows with phase-specific labels and color: pending/running, completed, and error. Permission requests replace the prompt area and use warning/error styling.

## Layout

The frame is split vertically into status, body, prompt, and footer. The body is split horizontally only on wide terminals.

- `width >= 110`: show transcript and right sidebar.
- `width < 110`: hide sidebar and keep transcript full width.
- `height < 20`: compress footer/help text and keep prompt fixed.

The sidebar is presentation-only. It summarizes session id, model, streaming state, goal, loop, team members, and pending permission. It does not introduce new commands.

## Data flow

`AppState::apply` continues to fold `Envelope` values into the existing `Projection`. Rendering adds a pure view-model step:

1. Read `AppState`.
2. Convert messages and parts into `TimelineItem` values.
3. Render status, timeline, optional sidebar, prompt, and footer.

The view-model allows tests to assert user/tool/sidebar behavior without depending on terminal I/O.

## File structure

- `crates/yaca-tui/src/lib.rs`: public `AppState`, view structs, `draw`, and module exports.
- `crates/yaca-tui/src/theme.rs`: semantic theme and style helpers.
- `crates/yaca-tui/src/view_model.rs`: projection-to-timeline conversion.
- `crates/yaca-tui/src/layout.rs`: responsive layout calculation.
- `crates/yaca-tui/src/widgets.rs`: ratatui rendering helpers for status, timeline, prompt, sidebar, and footer.
- `crates/yaca-tui/tests/tui_render.rs`: render regression tests using `TestBackend`.

## Compatibility

The first implementation should not change CLI key handling or engine events. Existing tests continue to compile, although assertions may be updated to match the new labels and layout. New modules stay inside `yaca-tui`; no store, provider, tool, server, or core crate changes are required.

## Error handling

Rendering functions avoid panics. Width and height math uses saturating operations. Empty or malformed tool inputs are rendered as compact labels rather than parsed ad hoc. Terminal dimensions below the comfortable minimum still show status and prompt without overlapping text.

## Testing

Tests use `ratatui::backend::TestBackend` and inspect buffer text. They cover:

- Empty state banner and footer.
- Wide sidebar rendering.
- Narrow layout without sidebar.
- User rail and assistant body styling.
- Tool completed/error labels.
- Permission prompt takeover.
- Scroll saturation.

## Risks

- Ratatui wrapping and width calculation can drift from test expectations. Mitigation: assert stable semantic labels rather than exact full frames.
- Sidebar may crowd narrow terminals. Mitigation: hide under 110 columns.
- Over-modularization can slow delivery. Mitigation: split only theme, layout, view-model, and widgets.
