# OpenCode parity worktree PR split

## Goal

Split the OpenCode parity review into independently reviewable worktree PRs with worker-agent ownership, TDD gates, and merge-order notes.

## Requirements

- Split the OpenCode parity gap into PR-sized changes that can be implemented in isolated worktrees.
- Keep each child PR independently reviewable, with a red test first, a bounded implementation scope, and a documented verification gate.
- Use worker agents only after each child task has a clear ownership boundary and disjoint business-code write set.
- Honor the project release rule by assigning each child PR a unique next version, while documenting the resulting changelog/version merge order.
- Do not attempt broad OpenCode parity in one PR. OAuth, desktop/IDE/cloud/share parity, and full provider-catalog parity remain follow-up roadmap items.

## Acceptance Criteria

- [ ] Four child tasks exist for version hygiene, Compat MCP import, TUI theme picker, and revert snapshot baseline.
- [ ] Each child task has PRD, design, and implement artifacts with branch, worktree, version, tests, and verification.
- [ ] Each child maps to one worktree under `.worktrees/` and one worker-agent assignment.
- [ ] PRs are opened separately with atomic commits and clear merge-order notes for the shared version/changelog files.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
