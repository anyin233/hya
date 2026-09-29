# 0.43.9

## Lazy pane focus rendering

- Keep mounted pane content and scroll positions when switching focus with Alt+arrows or `/layout focus`.
- Recalculate the visible split tree only when the layout, terminal width, or sidebar visibility changes; focus updates only the affected pane chrome and status.
