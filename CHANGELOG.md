# 0.33.31

- Fixed TypeScript TUI subagent observation exit: Escape again returns focus to the Main agent view even while the main turn is still busy (session.interrupt no longer steals Escape when the prompt is hidden).
- Escape on a child-session route walks back to the parent session.
