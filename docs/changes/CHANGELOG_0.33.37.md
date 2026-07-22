# 0.33.37

- Show all launched subagents in the main assistant message (OpenCode-style Task rows), including multi-member `task` calls, with live status from the run tree and child session activity.
- Task tool batch results now carry per-member `description`, `subagent_type`, and `sessionId` metadata for the TUI.
- Member spawn rows use the short task description (not the full prompt) so main-message status can match the live tree.
