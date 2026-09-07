# Task Plan

## Goal

Plan and then execute the smallest supportable migration of OpenCode's TUI to
`hya-tui-ts`, launched by `hya-ts` against hya's backend.

## Phases

- [complete] Establish requirements and inspect both runtime boundaries.
- [complete] Merge parallel architecture and migration proposals into Trellis artifacts.
- [complete] Obtain artifact approval and activate the Trellis task.
- [complete] Implement seven TDD compatibility and packaging slices.
- [complete] Resolve independent-check failures at the SDK package, Compat event, branding, and workflow boundaries.
  - [complete] Add real-backend permission lifecycle coverage and correct only the legacy SDK payload fields it proves wrong.
  - [complete] Add real-backend question reply/reject coverage and correct only the completion payload fields it proves wrong.
  - [complete] Remove the remaining shipped SDK server/Console source, branding, and mutable workflow-ref failures demonstrated by the rerun.
- [complete] Re-run the independent check and full verification gates.
- [complete] Update durable specs, commit and push only task-owned files, then finish the Trellis task.

## Decisions

- Import only the TUI runtime closure, not OpenCode's backend.
- Pin provenance to OpenCode 1.17.9 commit `cf31029350820c6bfc0fbd0e052a79a067ee6116`.
- Preserve the full upstream MIT notice while de-branding user-visible product text.
- Ship `hya-ts` alongside the existing `hya`; do not switch or remove the Rust frontend in this task.
- Require system Bun for the first release; defer single-file Bun/OpenTUI packaging.
- Remove OpenCode-backend-only controls instead of retaining placeholders.
- Reuse the Rust `ServerHandle`; keep Bun frontend-only.
- Preserve current in-memory backend defaults; do not add a persistence policy to this migration.
- Drive permissions through the existing non-yolo `session.shell` seam and questions through the existing configurable OpenAI-compatible provider seam; add no trigger endpoint.

## Errors

- Initial broad CodeGraph query returned incomplete TUI source coverage; use explicit source paths and package manifests for follow-up inspection.
- A shell-only source-count command had an unmatched quote; use dedicated file/search tools instead of repeating it.
- The configured `trellis-check` agent type was unavailable in this runtime; an isolated read-only general agent ran the same gate and returned `FAIL` for incomplete question reply/global envelopes, shipped dormant SDK server code, one branding leak, dead Console source, incomplete real-SDK lifecycle coverage, and mutable release action refs.
- The first permission serializer compile check failed because the new legacy view was not re-exported through `pending.rs`; added the same module re-export used by existing pending views before rerunning.
- The first question regression timed out while polling `/session/status` for an idle entry. That endpoint intentionally lists active runs only; changed the test to the existing streamed `session.status: idle` contract and kept provider-round completion as a separate assertion.
- The first rerun of the strengthened installer check stopped before fixture setup because `install.sh` did not invoke SDK pruning. After that correction it stopped only on `actions/checkout@v4`, proving the workflow-ref assertion independently.
