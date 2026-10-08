# 0.45.5

- Integrate concurrent native-bundle preparation, verified library caching, responsive VCS RPC scheduling, and direct native TUI supervision with batch-first subagent delegation.
- Prefer one `task` call with `tasks[]` and shared `context` for independent work, while retaining legacy single-task and `members[]` inputs and reporting running work accurately.
- Verify the combined implementation with schema/admission regressions, process E2E, browser startup/reload checks, and a real-provider pelican task with two batched review agents. The full 100ms cold / 50ms warm startup budgets remain unmet.
