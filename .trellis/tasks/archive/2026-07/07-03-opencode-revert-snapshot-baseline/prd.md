# Session revert snapshot baseline

## Goal

Introduce a first durable snapshot/revert baseline for session file changes so hya can start moving beyond metadata-only revert parity.

## Requirements

- Move session revert beyond metadata-only behavior for new file-changing tool events.
- Persist enough before/after snapshot metadata in tool results for revert and unrevert to restore edited files.
- Preserve existing diff/summary response behavior and busy/missing-session error behavior.
- Degrade safely for old events without snapshots by keeping the current metadata-only response.

## Acceptance Criteria

- [x] A red server integration test proved `/session/:id/revert` recorded metadata while leaving changed file content in place.
- [x] New `edit` tool events include before/after content snapshots for the changed file.
- [x] `/session/:id/revert` restores before snapshots and `/unrevert` restores after snapshots, including repeated edits to one file.
- [x] Revert deletes newly created files, snapshots preserve formatter output and UTF-8 BOMs, and relative workdirs restore correctly.
- [x] Restore paths reject outside-workdir, parent-traversal, and symlink-escape paths; old events without snapshots retain metadata-only behavior.
- [x] Existing revert API tests, the full Rust CI-equivalent gate, local binary builds, and version `0.33.14` metadata all pass.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
