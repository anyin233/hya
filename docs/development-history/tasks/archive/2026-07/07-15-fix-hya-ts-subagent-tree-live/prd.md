# Fix hya-ts subagent tree live view

## Goal

Make the current `hya-ts` subagent roster reliably load a live recursive tree instead of showing `Subagent tree unavailable - press r to retry`.

## Background

- The installed frontend is `hya-ts` version `0.33.7`.
- The user reports that opening the subagent roster still reaches the tree-load error state.
- The failure must be reproduced with a real `hya` root session that delegates project-summary work to multiple subagents, not only with a synthetic fixture.

## Requirements

- R1. Establish an unattended, repeatable check that detects the reported tree-load error at the live HTTP/frontend boundary.
- R2. Use `hya` to create one bounded root session and spawn at least two distinct subagents that summarize different parts of this repository.
- R3. Capture enough non-secret evidence to distinguish frontend parsing, HTTP routing/status, backend tree construction, and session-lineage failures.
- R4. Fix the shared root cause with the smallest change; do not add a second tree projection, endpoint, or compatibility path unless the reproduced evidence requires it.
- R5. Preserve recursive ancestry, roster metadata, read-only observation panes, retry behavior, and root-only prompt ownership.
- R6. Keep provider credentials, authorization headers, and raw prompt/provider payloads out of persisted diagnostics and user-visible reports.
- R7. Bound the live workload to the requested project-summary test and stop if unexpected permissions, provider failures, or unrelated mutations appear.

## Acceptance Criteria

- [x] AC1. The pre-edit checks deterministically exercise the visible retry state and the exact child-bearing HTTP/frontend boundary; a naturally passing current runtime is recorded as not reproduced after reinstall rather than treated as a product defect.
- [x] AC2. One live root session admits at least two child subagent sessions with distinct project-summary assignments.
- [x] AC3. `GET /session/{root}/tree` returns a parseable root tree containing every admitted child with session, status, agent type, and available roster metadata.
- [x] AC4. `hya-ts` opens the roster without the retry error, lists the live children, and opens one child as a read-only observation pane.
- [x] AC5. Pressing `r` after an induced or retained fetch failure performs one fresh request and recovers when the endpoint is healthy.
- [x] AC6. The focused regression test, complete TypeScript suite, typecheck/build, relevant Rust gate, and local executable build pass.
- [x] AC7. The live project-summary run completes or is cleanly stopped, and its root synthesis names the delegated repository areas without exposing secrets.
- [x] AC8. The no-source-change branch was inapplicable because the child-bearing current runtime reproduced the parser defect; task changes were committed and pushed only after explicit authorization.

## Out Of Scope

- Redesigning the subagent workspace or changing its approved pane/keybinding model.
- Benchmarking providers or expanding subagent governor limits.
- Fixing unrelated failures discovered outside the tree request and rendering flow.
