# 0.43.8

## `project_activity`: see who else is working in this Project

- New read-only agent tool `project_activity` (extended-tools family, permission `read_only`, allowed without prompting). It lists the other sessions and agents in the caller's Project — independent sessions, subagents, team members — with their relation to the caller (`parent`, `child`, `sibling`, `unrelated`), lineage root, busy/idle state, and last activity, plus the newest change per file (`created` or `changed`) with the session that made it.
- Agents can now check concurrent work in a shared worktree directly instead of inferring it from process lists, file timestamps, and `git diff`.
- Parameters: `since_ms` (default: last 2 hours), `limit` (default 50, max 200), `include_self` (default `false`). Results never include prompts, tool arguments, or file contents. `busy`/`idle` is the serving daemon's live turn state.
- One bounded store query serves the file list; the canonical tool registry now has 29 names.

Example tool call:

```json
{"name": "project_activity", "input": {"since_ms": 1790600000000, "limit": 20}}
```
