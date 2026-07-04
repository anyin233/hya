# 0.32.3

Subagent manager TUI release: adds read-only tmux-style observation for live Team subagents.

- **Replaced the Team Roster overlay entry point.** Ctrl+X O opens the Subagent manager; selecting a live subagent opens a read-only observation view scoped to the current Team.
- **Added tab and split placement.** The manager supports focused tabs plus vertical and horizontal observation splits, while preserving the main Prompt composer as the only user-input surface.
- **Closed finished observation views automatically.** Aux panes close on terminal team lifecycle events without retargeting the main input session.
- **Surfaced subagent attention.** The main session status line shows live subagent counts and pending permission/question attention.
- **Polished aux transcripts.** Read-only panes stay pinned when manually scrolled, show a persistent new-output marker, and clear it after returning to the bottom.
