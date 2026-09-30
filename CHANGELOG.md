# 0.43.37

- `/open <number>` counts the sessions the sidebar lists (the active Project's, plus temporary ones), so `/open 2` opens the sidebar's 2 even when other Projects have sessions. `/sessions` and pending-ask labels use the same numbers; a session the sidebar does not list (another Project's under F3, an archived one under Ctrl+A) shows without a number.
- `/sessions` rename (F2) edits the session's title, and a delete confirmation names it, without the list number (`1. `) or the subagent arrow.
- Docs: shell-turn and waiting tool cards show the compact layout (`◌ bash  awaiting approval` over the JSON arguments).
- Tests: the WebUI specs follow the compact tool cards, numbered session rows, the 150-column right sidebar, and a TUI being a supervisor plus its app.
