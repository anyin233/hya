# OpenCode parity worktree PR split

## Goal

Deliver the remaining OpenCode parity slices as independently reviewable, verified PRs with TDD evidence and an explicit merge order.

## Requirements

- Split the OpenCode parity gap into PR-sized changes with isolated branches and reviewable diffs.
- Keep each child PR independently reviewable, with a red test first, a bounded implementation scope, and a documented verification gate.
- Honor the project release rule by assigning each child PR a unique sequential version and encoding the dependency order in the PR bases.
- Do not attempt broad OpenCode parity in one PR. OAuth, desktop/IDE/cloud/share parity, and full provider-catalog parity remain follow-up roadmap items.

## Acceptance Criteria

- [x] Four child tasks exist for version hygiene, Compat MCP import, TUI theme picker, and revert snapshot baseline.
- [x] Each child task records its branch, base, version, red test, implementation, verification gate, commit, and PR.
- [x] Each feature PR has an atomic implementation commit and passed the full Rust CI-equivalent gate plus local binary builds.
- [x] The PRs form the mergeable stack `#7 -> #9 -> #8 -> #10 -> #11`, so version and changelog history remain linear.
- [x] The merge procedure requires retargeting each successor to `main`, inspecting the reduced diff, and waiting for fresh checks after its dependency merges.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
