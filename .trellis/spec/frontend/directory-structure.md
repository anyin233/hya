# Directory Structure

> How frontend code is organized in this project.

---

## Overview

The frontend layer is split between the app-specific Rust terminal UI in
`crates/hya-tui` and the reusable primitive library in `crates/hya-tui-lib`.
`hya-tui` owns the current TUI runtime: terminal setup/restore, crossterm
events, async app loop, prompt/keymap/theme/screen behavior, panes, widgets,
and Hya-specific rendering. `hya-tui-lib` owns only app-neutral geometry, color,
flex layout, overlay/layer validation, declarative component descriptors, and
ratatui adapter helpers.

For TUI work, keep app presentation modules inside `crates/hya-tui/src/`:

- `lib.rs` exposes public app-facing state types and draw entry points.
- `contracts.rs` keeps app-specific input/keymap/prompt document contracts and
  compatibility re-exports for shared primitives from `hya-tui-lib`.
- `render/` owns app text/style adapters and compatibility re-exports for shared
  flex/overlay/draw primitives.
- `theme.rs`, screen modules, widgets, and prompt modules remain app-specific.

Reusable primitives that have no dependency on Hya app state belong in
`crates/hya-tui-lib/src/`, not in `hya-tui`.

---

---

## Directory Layout

```
crates/hya-tui-lib/src/
├── lib.rs
├── contracts.rs
├── component.rs
├── layer.rs
└── render/
    ├── draw.rs
    ├── flex.rs
    ├── mod.rs
    └── overlay.rs

crates/hya-tui/src/
├── lib.rs
├── app.rs
├── tui.rs
├── contracts.rs
├── app/
├── keymap/
├── render/
├── screens/
├── state/
├── theme/
├── widgets/
└── prompt/

```

---

## Module Organization

Add app-specific TUI runtime or presentation behavior to the smallest matching
`crates/hya-tui/src/` module. `hya-tui` may reference Hya app state, terminal
I/O, crossterm events, async tasks, prompt behavior, keymaps, themes,
provider/model concepts, sessions, and screens.

Add generic layout/component/layer/geometry primitives to `hya-tui-lib` only
when they do not reference Hya runtime crates, app state, terminal I/O, async
tasks, prompt behavior, keymaps, themes, provider/model concepts, or screens.

---

## Naming Conventions

Use short, responsibility-based module names. Prefer `snake_case` for files and
types that describe render concepts (`TimelineItem`, `AppLayout`, `Theme`).

---

## Examples

- `crates/hya-tui-lib/src/render/flex.rs` for app-neutral flex layout solving.
- `crates/hya-tui-lib/src/component.rs` for declarative component trees and layer claims.
- `crates/hya-tui/src/contracts.rs` for Hya-specific input/keymap/prompt contracts.
- `crates/hya-tui/src/render/draw.rs` for app text/style conversion plus shared draw-adapter re-exports.
