# 0.43.34

- The TUI's right sidebar (Sessions, Todos, Context) needs 150 columns; below that it is hidden and the top status line shows its Context fields. It is never drawn narrower than 29 columns, 60% wider than the 18 columns its default share gave at 149 columns.
- Drag the border between side-by-side panes with the mouse to resize them: the right sidebar's left border, the Projects sidebar's right border, or any vertical `/layout split`. The panes follow the pointer, and the new width is saved with the layout on release.
