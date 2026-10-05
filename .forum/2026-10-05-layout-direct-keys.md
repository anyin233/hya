# How do direct Layout editor keys distinguish the cursor and editing target?

**Answer:** LayoutEditorState.selected is the cursor; marked is one optional node id, drawn as ◆ beside the cursor's ▸. Shift+Enter or Space toggles/replaces the mark. i always inserts before the cursor (root asks a child position); w then r/c wraps the mark or cursor, defaults after, with Tab before/after. Backspace/Delete directly remove an auxiliary leaf; groups show Cancel-first confirmation. Viewer/editor and their ancestor groups remain protected. Marks survive navigation and focus changes, and disappear when removed/normalized away. Insert/wrap select the added leaf; removal prefers next sibling, previous sibling, parent, then surviving root. Existing menu operations retain their behavior.

Pending wrap/chooser/removal targets live in the transient editor stage. External removals cancel stale targets; don't silently retarget to the repaired cursor. All mutations reuse ordered-tree reducers, save the same v4 paneLayout and preserve Layout pane input focus. Direct keys never operate in text forms or other panes. Help rows feed inheritedBindingRows so keys are inspectable/removable with the existing keybind settings.

## How does Shift+Enter marking work in the browser?

**Answer:** The existing generic --shift-enter-lf host mode sends LF. Layout tree marking accepts native Shift+Enter, LF/Ctrl+J and Space. Keep these aliases visible in help and docs; Space is portable on terminals lacking enhanced key reporting. Don't add hya-specific key interpretation to the generic WebUI host. Browser tests use press(Shift+Enter) through the real PTY; units cover native modifiers and aliases.

Coverage: layout-editor and keybinding-inventory units; hya-tui-layout-direct-keys browser specs at 1100/690 px including insertion/wrap placement, mark versus cursor, removal confirmations/protection, draft isolation, persistence, focus switches, resize, cancellation, exit, and /keybind unset I/reset I. Shortcuts in command completion use canonical I, so lower-case i can require an extra Enter for completion. Artifact logs/PNGs under ~/data/hya-rust/tmp/layout-direct-*.
