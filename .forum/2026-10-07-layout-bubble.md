# How should adjacent layout reordering preserve sizes and tree identity?

Use `bubblePane` to swap complete sibling `PaneChild` slots directly. General `movePane` detaches then normalizes, which is appropriate for changing containers but unnecessary for an adjacent swap. Direct swaps retain weighted/content sizes, viewer pins, stable group IDs, descendants and active focus. Root and first/last boundary moves return the original layout.

In the Layout pane, Shift+Up/Down bubbles the cursor node (independent of a mark); Enter actions exposes Bubble previous/next. `/layout bubble <node-id|pane-name|root> <previous|next>` uses the same reducer. Row order maps to left/right and column order to up/down. The editor keeps its focus and selected node after the move.

Browser coverage checks normal and narrow widths, row/group and column/pane movement, unchanged size slots and marks, retained drafts and persistence. Launching another `tui()` in one fixture closes the previous client; assert its local draft before relaunching.
