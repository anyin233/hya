# 0.43.53

- Introduce layout v4 ordered row/column containers, with migration of saved binary layouts, content sizing and safe normalization.
- Add `/layout tree`, `insert`, `move`, `wrap` and `remove` commands for uniform branch editing.
- Make Alt+Left/Right cycle through every visible selectable pane in visual order; Alt+Up/Down choose nearby panes using drawn bounds.
- Preserve mounted pane state, drafts and transcript scroll across structural edits and reloads, and support dragging row and column boundaries.
