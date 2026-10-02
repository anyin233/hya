# 0.45.1

## Task execution

- Multi-member `task` calls register all resident subagents concurrently, so a slow member no longer blocks the rest of the batch.

## Runtime and tooling

- Improve runtime member registration and tool-call presentation, including semantic argument summaries for builtin tools.
- Keep project/session startup and project switching state synchronized across the TUI and backend.
