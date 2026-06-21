# TUI

The terminal UI is split between:

- [`yaca-cli/src/tui.rs`](../../crates/yaca-cli/src/tui.rs): terminal I/O and
  async event loop.
- [`yaca-tui`](../../crates/yaca-tui): pure ratatui state, layout, theme,
  view-model conversion, and widgets.

## CLI Event Loop

The CLI TUI owns side effects:

- terminal raw mode
- alternate screen enter/leave
- panic hook that restores the terminal before printing panic output
- keyboard input
- spawning assistant turns
- subscribing to the engine event bus
- receiving permission ask requests

On startup it creates a session, primes the projection, enters the alternate
screen, and draws the initial UI. Each submitted prompt runs in a spawned task:

1. admit the user prompt
2. run one assistant turn
3. inject a system message if prompt admission or turn execution fails
4. notify the UI loop that the turn completed

## Renderer Crate

`yaca-tui` is intentionally free of terminal I/O. Its public entrypoint is
[`draw`](../../crates/yaca-tui/src/lib.rs), which composes four internal
modules:

| Module | Responsibility |
| --- | --- |
| [`layout.rs`](../../crates/yaca-tui/src/layout.rs) | Computes status, timeline, optional sidebar, prompt, and footer rectangles. |
| [`theme.rs`](../../crates/yaca-tui/src/theme.rs) | Defines the dark color palette and base style. |
| [`view_model.rs`](../../crates/yaca-tui/src/view_model.rs) | Converts `Projection` messages into timeline items. |
| [`widgets.rs`](../../crates/yaca-tui/src/widgets.rs) | Renders the status bar, timeline, sidebar, prompt, footer, permission panel, and cursor. |

## Renderer State

[`AppState`](../../crates/yaca-tui/src/lib.rs) holds:

- current `Projection`
- optional goal, loop, team, and permission views
- input buffer
- running flag
- scrollback
- model label
- session label

`AppState::apply` folds an incoming `Envelope` into the projection. The renderer
does not know where the envelope came from.

## Layout

The view is a responsive terminal layout:

1. one-line status bar
2. scrollable timeline
3. input box
4. one-line footer

When the timeline region is at least 110 columns wide, the renderer reserves a
right sidebar for context. Otherwise the timeline uses the full width.

If a permission request is active, a modal-like permission panel is drawn near
the bottom.

## Input

General keys:

| Key | Behavior |
| --- | --- |
| `Enter` | Submit input if no turn is running and input is not blank. |
| `Backspace` | Delete one character. |
| `PageUp` / `PageDown` | Scroll five lines. |
| `Up` / `Down` | Scroll one line. |
| `Esc`, `Ctrl-C`, `Ctrl-D` | Quit. |

Permission panel keys:

| Key | Behavior |
| --- | --- |
| `Left` / `Right` / `Tab` | Move selected decision. |
| `Enter` | Confirm selected decision. |
| `Esc` | Deny. |
| text input | Add optional rejection feedback. |

## Rendering Tool Calls

Tool parts render as compact lines with:

- tool name
- truncated input
- status
- elapsed time for completed calls
- error summary for failed calls

Reasoning parts become a compact `Thinking` timeline marker when they contain
non-empty text.

## Test Boundary

Because `yaca-tui` is pure rendering, tests in
[`../../crates/yaca-tui/tests`](../../crates/yaca-tui/tests) can render states
into buffers without opening a real terminal. The CLI event loop remains covered
separately by the runtime paths it exercises.
