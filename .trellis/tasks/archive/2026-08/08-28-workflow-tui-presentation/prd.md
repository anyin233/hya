# Implement Workflow TUI Presentation

## Outcome

Show durable Workflow state in the existing TypeScript TUI sidebar plugin system, driven only by synchronized typed Session state.

## Requirements

- Register one hya-owned `sidebar_content` built-in; do not add a roster sidebar, status line, provider, reducer, client, or polling loop.
- Hydrate Workflow state through the existing bootstrap/Session data and replace it on existing `session.updated` events.
- Show selection/revision availability, run status, active/total Agent instances, completed/total Stages, graph level, active Stage ids, and bounded current work.
- Use server-derived Workflow activity so unrelated run-tree Agents do not affect counts.
- Reuse existing run-tree/roster observation and existing server `/workflow` command flow.
- Follow `DESIGN.md`, semantic theme tokens, fixed sidebar width, plugin ordering, clipping, and cleanup.

## Acceptance Criteria

- [x] The sidebar renders none, ready, running fan-out, completed, failed, cancelled, interrupted, stale, and unavailable states deterministically.
- [x] Parallel Stages show declaration-ordered names with compact `+N`; current work truncates before identity/count fields.
- [x] Bootstrap and SSE update the same state with no timer, poll request, or second SDK client.
- [x] Counts ignore unrelated Team/run-tree members and child navigation continues through the existing roster.
- [x] Plugin registration/unregistration and narrow/wide layout tests pass.
- [x] Actual PTY verification observes selection, running, terminal, and restored states in the real TUI.

## Exclusions

- No command grammar fork, local DAG, backend state reducer, or new Workflow execution behavior.
