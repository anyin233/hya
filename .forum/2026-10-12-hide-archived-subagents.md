# How should archived subagents behave in the TUI?

Archived actors are hidden from the selector, command completion, and transcript viewer panes by default. Task completion or idle alone does not hide them. Viewer pins remain in layout state and become live again on revival; a following viewer moves to another visible child or clears. Active descendants remain visible even if an ancestor is archived.

`subagentRows` filters by `archived === true` by default. Background controller discovery and lifecycle matching explicitly use `{ includeArchived: true }`: otherwise hidden nested agents would not be refreshed when root notifications revive them. The API projection and history remain intact.

The follow-up adds Ctrl+A in the selector and `/subagents archived [show|hide|toggle]`.
`subagentsShowArchived` is conversation-local frontend state (default false), shared
by selectors, viewers, and completion. It resets on switching conversations or
relaunching, and never writes archive state to the backend. Contextual help rows
make the shortcut discoverable through `/keybind`; the existing central router
handles unset and overrides before pane input.
