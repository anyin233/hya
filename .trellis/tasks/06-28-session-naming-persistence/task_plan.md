# Session naming and persistence task plan

## Goal

Implement hya session naming and SQLite persistence exactly within the user request, with OpenCode-compatible naming semantics and no scope expansion.

## Phases

1. Planning and evidence: create Trellis task, gather hya/OpenCode context, write PRD/design/implementation artifacts.
   - Status: complete
2. Plan review and task activation: merge parallel planner findings, run cross-model plan review, start Trellis task.
   - Status: complete
3. TDD implementation: add regression coverage for `hysec_` IDs, SQLite-backed headless persistence, exact session listing, and replay.
   - Status: complete
4. Production changes: route headless commands through the selected store and parse CLI session IDs with the shared parser.
   - Status: complete
5. Verification and manual QA: run focused and workspace gates plus real CLI surface checks.
   - Status: complete

## Errors Encountered

| Error | Attempt | Resolution |
| --- | --- | --- |
| Initial plan review failed D1/D2/D3/D5 | First merged plan had unspecified future ID grammar, coarse waves, unnamed title sentinel, and prose verification commands | Wrote `design.md` and `implement.md` with exact `hysec_[A-Za-z0-9]{20}` contract, crate-local waves, canonical fallback title, and literal cargo commands |
| Plan-review Round 1 failed D2/D5 | `implement.md` hid empty sessions but did not name a concrete finalization cleanup hook or prove direct lookup is not found after cleanup | Added backend TUI finalization cleanup tasks 3.5/3.6, direct GET/store lookup verification, explicit cleanup abort gates, and manifest references |
| Plan-review Round 2 failed D2/D3/D5 | Cleanup helper ownership was backend-only but server tests needed to invoke it, and `prd.md` still had a stale OpenCode title open question | Moved cleanup helper ownership into `hya-core`/`SessionEngine`, made backend/server tests call the shared helper, and marked OpenCode title semantics resolved in `prd.md` |
