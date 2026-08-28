# Implementation Plan

Follow strict red-green order. Run only the named focused test between edits; run the child gate after all contracts are green.

1. Add a failing `hya-workflow` compile test for the smallest linear Markdown document and exact normalized order. Create the crate and compiler interface.
2. Add failing grammar cases for fan-out/fan-in, comments, standalone nodes, invalid tokens, missing/duplicate nodes, self/cycle edges, and source line/column. Implement the restricted parser and planner.
3. Add failing input/join tests. Implement required `{{input.*}}` validation and automatic 4,000-byte UTF-8-safe direct-predecessor evidence.
4. Add failing loop/verifier/actor validation tests and canonical revision determinism. Implement normalized modes and hashes.
5. Migrate current core transient Workflow tests to compile through the new interface; delete direct serde/old author fixtures.
6. Add an overlap test that blocks two same-level provider calls at a barrier. Wire compiled levels through the existing governed batch until it passes.
7. Add resident red tests: sequential actor reuse, first directive as mail, same-level collision, different actor keys with one Agent, failure, cancellation, and target context. Retain `spawn_lifecycle`, integrate `ResidentSupervisor`, and await Projection cursor/work boundaries.
8. Update failure policy so collect-all continues but the run is Failed when any Stage failed; pin pending/terminal Stage reports.
9. Migrate backend/app/E2E source fixtures enough to compile through `hya-workflow`; leave durable control behavior unchanged.
10. Remove obsolete parser/planner exports, aliases, dependencies, and documents owned by this child.
11. Run both mutation checks from the parent plan, revert them, then run the focused child gate.
12. Run Trellis check review and fix all findings before finishing the child.

Rollback: edits remain uncommitted on top of the pushed `0.35.2` baseline. If the compiler seam is invalid, revert only this child's owned files; do not restore the old public format as a fallback.
