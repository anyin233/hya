# How should archived subagents behave in the TUI?

Archived actors are hidden from the selector, command completion, and transcript viewer panes by default. Task completion or idle alone does not hide them. Viewer pins remain in layout state and become live again on revival; a following viewer moves to another visible child or clears. Active descendants remain visible even if an ancestor is archived.

`subagentRows` filters by `archived === true` by default. Background controller discovery and lifecycle matching explicitly use `{ includeArchived: true }`: otherwise hidden nested agents would not be refreshed when root notifications revive them. The API projection and history remain intact.

The user deferred an interactive show/hide keybinding to a later commit. Do not add a toggle as part of this change.
