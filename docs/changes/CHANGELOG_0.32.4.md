# 0.32.4

TUI session reset and subagent visibility fixes.

- **Reset `/new` cleanly.** The Session screen now aborts the old active turn asynchronously, clears queued and submitted prompt bookkeeping, resets history and panes, and keeps new sessions lazily created on the next fresh prompt.
- **Preserved first lazy submit ownership.** Home-route lazy session creation now keeps first-submitted prompts as the owner even when later `/session` responses complete first, retracts stale optimistic prompt rows, and replays startup queued prompts into one created session in order.
- **Rendered subagent lifecycle rows.** Member spawn and terminal events now appear as compact derived timeline rows without polluting stored message transcripts or copy/export output.
- **Scoped team surfaces to the active Team root.** The sidebar, Subagent manager, and Channels & inboxes overlay resolve child routes through the root Team, hide the main/root entry where actionable, and keep same-handle rosters distinct across teams.
