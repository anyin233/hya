# Component Guidelines

> How components are built in this project.

---

## Overview

TUI components are ratatui rendering helpers, not stateful UI objects. They
should accept `&AppState` or derived view-model values plus a `Theme`, render
into a `Frame`, and avoid terminal I/O.

The public API should remain small: `AppState`, feature view structs, and `draw`
are exposed from `lib.rs`; lower-level widgets stay crate-private unless another
crate has a real need.

---

## Component Structure

Widget helpers follow this shape:

```rust
pub fn render_status(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(Paragraph::new(status_line(state, theme)), area);
}
```

Keep layout math out of widget helpers. Compute `Rect`s in `layout.rs` and pass
the final area into the renderer.

---

## Props Conventions

Prefer explicit inputs over hidden globals. Pass `Theme` by reference, pass
`Rect` by value, and pass derived view-model structs when the widget does not
need all of `AppState`.

---

## Styling Patterns

Styles come from semantic theme fields such as `background`, `panel`, `text`,
`muted`, `warning`, and `error`. Do not scatter raw `Color::Rgb` values through
widgets; add a semantic field to `Theme` when a new color role is needed.

---

## Accessibility

Terminal accessibility is mostly contrast and text fallback. Do not rely on
color alone for important state; include text labels such as `streaming`,
`permission`, `completed`, or `error`.

---

## Common Mistakes

- Do not put crossterm input handling in `yaca-tui`.
- Do not parse provider/tool business logic in widgets; use the projection and
  view-model layers.
- Do not compare whole-screen snapshots for routine layout tests when semantic
  text assertions are enough.
