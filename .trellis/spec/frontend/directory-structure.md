# Directory Structure

> How frontend code is organized in this project.

---

## Overview

The frontend layer is the Rust terminal UI in `crates/yaca-tui`. It is a pure
ratatui rendering crate: it receives `AppState` and paints a frame. Terminal
setup, crossterm events, async tasks, cancellation, and model streaming stay in
`crates/yaca-cli/src/tui.rs`.

For TUI work, keep presentation-only modules inside `crates/yaca-tui/src/`:

- `lib.rs` exposes public state types and `draw`.
- `theme.rs` owns semantic colors and shared styles.
- `layout.rs` owns responsive `Rect` calculation.
- `view_model.rs` converts protocol projections into renderable timeline items.
- `widgets.rs` renders status, timeline, sidebar, prompt, and footer widgets.

`crates/yaca-cli` should not grow view-model or ratatui widget code. `crates/yaca-tui`
should not own terminal raw mode, input polling, network calls, or engine events.

---

---

## Directory Layout

```
crates/yaca-tui/src/
├── lib.rs
├── layout.rs
├── theme.rs
├── view_model.rs
└── widgets.rs
```

---

## Module Organization

Add new TUI presentation behavior to the smallest matching module. Create a new
module only when a file would otherwise mix unrelated responsibilities, such as
diff rendering, markdown rendering, or modal/dialog rendering.

---

## Naming Conventions

Use short, responsibility-based module names. Prefer `snake_case` for files and
types that describe render concepts (`TimelineItem`, `AppLayout`, `Theme`).

---

## Examples

- `crates/yaca-tui/src/layout.rs` for responsive terminal geometry.
- `crates/yaca-tui/src/view_model.rs` for projection-to-render conversion.
