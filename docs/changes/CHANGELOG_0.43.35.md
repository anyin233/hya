# 0.43.35

## Features

- The Commands dropdown now shows up to twelve recommendations, while fitting short terminals and scrolling through every match.

- `hya serve restart` now hot-updates the TUI as well: once a TUI (the terminal one and every WebUI tab) has attached to the new daemon, it starts itself again from the TUI files on disk, so new TUI features apply without quitting `hya`. The open session and the unsent composer text carry over; the reloaded TUI says `TUI reloaded (hya serve restart)`. Crash recovery, `/reconnect`, and remote backends do not reload. See docs/tui.md "Hot update after `hya serve restart`".
