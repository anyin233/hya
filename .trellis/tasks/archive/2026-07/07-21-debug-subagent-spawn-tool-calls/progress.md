# Progress

## 2026-07-21

- Created Trellis task `07-21-debug-subagent-spawn-tool-calls` in planning state.
- Identified two prior subagent E2E tasks as primary historical evidence.
- Began tracing the model-to-tool-to-spawner flow; no implementation changes made.
- Confirmed that current-source `0.33.2` previously completed a nested `task` invocation end to end, narrowing this investigation to a newer regression or a current-session/model contract mismatch.
- Anchored the current failure to root session `hysec_860vXRv1C6o1K2DKocPb` on the live `0.33.16` backend and recorded its failing `task_id` values.
- Traced the immediate failure to `TaskTool::execute`: every present `task_id` is parsed before spawn, so `""` and `"new"` cannot represent a new task.
- Kept provider strict-schema behavior and sibling optional-field handling open pending source inspection; no implementation changes made.
- Confirmed the OpenAI provider performs no strict-schema conversion and forwards the canonical optional-field schema unchanged.
- Identified the next discriminator: recover the failed calls' full arguments and determine whether eager `task_id` validation rejected a value the multi-member path would discard.
- Attempted to re-query the live root-session event endpoint; the server had exited and the connection was refused. Logged the evidence-recovery fallback in `task_plan.md`.
- Recovered the saved filtered event array from the local OpenCode conversation store.
- Proved both failures were batch calls whose top-level `task_id` would be discarded; eager validation is the root cause and branch-local validation is the smallest fix.
- Verified sibling empty model/category/inline values are ignored or trim-filtered in the resolver and do not require changes for this defect.
- Recorded the mandatory `0.33.17` version/changelog update and full Rust verification gate for planning.
- Completed four independent planner reviews; all support the minimal branch-local parse and focused regression test.
- Captured the dirty-work baseline and additional `0.33.16` mirrors before finalizing the release-file scope.
- Followed the latest release precedent and included README/private-package version mirrors in the `0.33.17` plan.
- Wrote `design.md`, `implement.md`, and curated implementation/check context manifests; no production code changed.
- First artifact validation failed only on two nonexistent generic spec paths; replaced them with the actual backend spec paths for revalidation.
- Trellis context validation passed with three focused entries in each manifest.
- Recorded newly appeared, non-overlapping `hya-core` and `hya-server` edits as protected concurrent work.
- Corrected the rendered-context check to use the current phase/step CLI after obsolete mode names were rejected.
- Resolved the current numeric Phase Index IDs (`2.1` implementation, `2.2` check) after descriptive step names were also rejected.
- Confirmed both Phase 2 step contexts render successfully with the curated manifests; planning is ready for user review.
- User reviewed the validated artifact summary and explicitly approved task activation and implementation.

## 2026-07-22

- Revalidated the task artifacts successfully and confirmed `task.json` is `in_progress`.
- Stopped at the Phase 0 baseline gate before RED: concurrent work already changed the complete planned release set to `0.33.17`, archived `CHANGELOG_0.33.16.md`, and replaced root release notes with unrelated websearch/Grok Build changes.
- No regression test, production source, release metadata, staging, commit, or push was performed. Continuing requires an explicit release-version/changelog decision.
- User chose to wait for the concurrent `0.33.17` release to land; the next session should revalidate the workspace and revise this task to `0.33.18` before RED.
- User resumed the task after the workspace advanced to a clean `0.33.18` release baseline.
- Revalidated source, test seam, shared metadata, and concurrent paths; revised the approved release target to `0.33.19` before RED.
- Added `task_batch_ignores_invalid_top_level_task_id` at the `TaskTool::execute` to `SpawnerPlane` boundary with the reproduced empty top-level `task_id` and two batch members.
- RED: `cargo test -p hya-tool --test task task_batch_ignores_invalid_top_level_task_id -- --exact` failed before spawner dispatch with `Input("invalid task_id: invalid session id: invalid length: found 0")`.
- GREEN: the same exact test passed after moving top-level `task_id` parsing into the single-task branch; `cargo test -p hya-tool --test task` then passed all 7 tests, including valid resume forwarding and malformed single-task rejection.
- Updated release metadata to `0.33.19`, archived the prior root notes as `docs/changes/CHANGELOG_0.33.18.md`, and confirmed `cargo check -p hya-tool` changed only workspace package versions in `Cargo.lock`.
- Verification passed: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo build --locked -p hya -p hya-backend`.
- Both built binaries report `0.33.19`; `git diff --check` passed, the archived changelog is byte-identical to the previous root changelog, and final status review confirmed all unrelated provider/config/spec work remains untouched.
- Independent `trellis-check` review found no defects and repeated the focused tests, full Rust gate, locked build, version smokes, changelog comparison, and diff integrity checks successfully.
- Captured the mode-dependent validation contract in `.trellis/spec/backend/task-tool.md` and linked it from the backend spec index without modifying the concurrently owned quality spec.
- Committed the verified atomic change as `2fa6a60f` (`fix(hya-tool): ignore unused batch task ids`); protected concurrent work remained unstaged.
