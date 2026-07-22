# 0.33.34

- Normalize empty/sentinel `task` `task_id` values (`""`, `new`, `null`, `none`) to a fresh spawn so models no longer fail resume validation when starting new subagents.
- Cap every tool result (builtin, MCP, plugin) to a notice plus the last 5000 characters so oversized `find`/explore outputs cannot blow the next model context window.
- Count tool and reasoning parts in compaction token estimates so tool-heavy turns actually trigger auto-compact.
- For OpenAI Responses / Codex / Grok Build routes, auto-compact via upstream `POST /responses/compact` and re-inject the returned window; other routes keep the local model summarizer fallback.
