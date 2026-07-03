# Session revert snapshot baseline

## Goal

Introduce a first durable snapshot/revert baseline for session file changes so hya can start moving beyond metadata-only revert parity.

## Requirements

- Move session revert beyond metadata-only behavior for new file-changing tool events.
- Persist enough before/after snapshot metadata in tool results for revert and unrevert to restore edited files.
- Preserve existing diff/summary response behavior and busy/missing-session error behavior.
- Degrade safely for old events without snapshots by keeping the current metadata-only response.

## Acceptance Criteria

- [ ] A red server integration test proves `/session/:id/revert` currently records revert metadata but leaves the changed file content in place.
- [ ] New `edit` tool events include before/after content snapshots for the changed file.
- [ ] `/session/:id/revert` restores the before snapshot for matching target files and `/unrevert` restores the after snapshot.
- [ ] Existing diff, summary, part-scoped revert, missing-session, and busy-session tests keep passing.
- [ ] Assigned version `0.29.6` release metadata is updated.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
