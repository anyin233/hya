# Component Guidelines

> How components are built in this project.

---

## Overview

TUI components split into two categories:

- App-specific render helpers live in `crates/hya-tui`. They accept app state or
  derived view-model values plus a `Theme`, render into a ratatui `Frame`, and
  avoid terminal I/O.
- App-neutral layout primitives live in `crates/hya-tui-lib`. They expose
  `Rect`, `Rgba`, flex layout contracts, layer validation, declarative
  `Component` trees, overlay helpers, and ratatui adapter functions without
  depending on Hya app state, prompt/keymap contracts, themes, screens, SDK,
  async runtime, or crossterm event loops.

Expose reusable primitives from `hya-tui-lib` only when another crate has a real
need. Keep lower-level Hya widgets crate-private unless they are app-neutral and
belong in the library.

---

## Component Structure

App widget helpers follow this shape:

```rust
pub fn render_status(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(Paragraph::new(status_line(state, theme)), area);
}
```

Keep layout math out of widget helpers. Compute `Rect`s in layout code and pass
the final area into the renderer. When layout math is reusable and app-neutral,
move it to `hya-tui-lib` and keep `hya-tui` compatibility paths as re-exports.

Reusable component trees use typed layout and layer contracts:

```rust
let layout = Component::container(FlexSpec { direction: FlexDirection::Row, ..Default::default() })
    .children([
        Component::leaf(NodeId(1), FlexSpec { grow: 1.0, ..Default::default() }),
        Component::leaf(NodeId(2), FlexSpec { grow: 1.0, ..Default::default() }).layer(LayerId(1)),
    ])
    .layout(area)?;
```

Same-layer overlaps must return a typed layer/component error. Intentional
overlap belongs on a distinct `LayerId` or through explicit overlay helpers.

---

## Props Conventions

Prefer explicit inputs over hidden globals. Pass `Theme` by reference, pass
`Rect` by value, and pass derived view-model structs when an app widget does not
need all of `AppState`. For library components, use `NodeId`, `FlexSpec`,
`LayerId`, and typed `ComponentError`/`LayerError` instead of stringly IDs or
silent rectangle lookup failures.

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

- Do not put crossterm input handling in `hya-tui` or `hya-tui-lib`.
- Do not parse provider/tool business logic in widgets; use projection and
  view-model layers.
- Do not compare whole-screen snapshots for routine layout tests when semantic
  text assertions are enough.
- Do not add Hya app-state, prompt, keymap, theme, SDK, or runtime dependencies
  to `hya-tui-lib`; keep that crate app-neutral.
- Do not allow same-layer component overlap silently; return typed validation
  errors and require explicit layers/overlays for intentional overlap.
