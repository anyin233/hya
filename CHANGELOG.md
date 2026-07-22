# 0.33.33

- Restarting a subagent on an existing session (`task_id` resume) reuses the original member id and roster handle so the subagent roster shows a single entry instead of duplicates that multi-highlight on select.
- The run tree collapses historical duplicate member rows that share the same child session (later state wins).
