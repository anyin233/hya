# Set up Matt Pocock skills

## Goal

Configure the repository's engineering skills to use its documented issue tracker, triage labels, and domain-document layout.

## Requirements

- Record GitHub Issues as the project tracker and keep external pull requests outside the triage request surface.
- Use the five canonical triage labels documented in `docs/agents/triage-labels.md`.
- Use the single-context domain layout: root `CONTEXT.md` plus root `docs/adr/`.
- Point the repository agent instructions at the corresponding `docs/agents/` files.

## Acceptance Criteria

- [ ] `CLAUDE.md` references the configured issue tracker, triage labels, and domain docs.
- [ ] `docs/agents/issue-tracker.md`, `triage-labels.md`, and `domain.md` describe the repository's selected conventions.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
