# When should the TUI draw an enclosing border?

**Question:** Can a pending notification, warning, transcript tool card, or extension structural box have an enclosing frame?

**Answer:** Enclosing frames denote keyboard selectable UI. Passive pending summaries, warning docks (answered through the composer), transcript tool/task blocks, and passive extension containers use plain headings and text. Left gutters and output dividers are permitted because they do not enclose a box. Selectable panes, inputs, and keyboard-owning overlays retain their frames.

The extension RenderNode `box` contract still accepts `border?: boolean` for compatibility, but the renderer ignores it and prints `title` as a text row. Pane frames belong to the host. Removing two border rows and adding one heading also requires reducing the composer dock minimum by one row.

Regression coverage: `hya-tui-passive-notices.spec.ts` reproduces a pending request with `/mew` at normal and narrow widths; `hya-tui-extension.spec.ts` covers a legacy `border: true` structural box. Browser artifacts live under `~/data/hya-rust/tmp/passive-notices/`.
