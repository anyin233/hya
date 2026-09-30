# 0.43.36

## Features

- Exactly one focus border follows keyboard ownership across the composer, workspace panes, Commands, Help, full-screen views, and provider forms.

- TUI session numbers are hierarchical: top-level sessions count `1`, `2`, …, and a subagent's session carries its parent's number (`2.1`, `2.1.3`) in the sidebar, `/sessions`, pending-ask labels, and `/open <number>`.
- `/open` and `/resume` completions show a titled session as `title (id)`, match its title as well as its id, and insert the id.
- `/rename` updates the session's row in the sidebar and in `/open` completion at once.
- `/new` run from the focused Projects sidebar hands focus back to the composer, so typing goes to the new session's prompt.
