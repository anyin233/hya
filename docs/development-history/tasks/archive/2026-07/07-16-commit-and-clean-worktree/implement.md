# Implementation Plan

## 1. Finish Planning

- [x] Obtain Trellis task-creation consent and create the task.
- [x] Inventory tracked/untracked paths and upstream state.
- [x] Run four independent planners and four focused ownership inspections.
- [x] Resolve local planning-record policy with the user.
- [x] Write converged PRD, design, execution plan, and real context manifests.
- [x] Validate artifacts.
- [x] Obtain implementation approval.
- [x] Start the task only after approval.

## 2. Prepare Publishable Content

- [x] Add the `.planning/` ignore rule and verify records remain on disk but leave status.
- [x] Correct ADR 0006 to match successful versus failed/cancelled lifecycle rows.
- [x] Correct the stale ADR-0007 example in `docs/agents/domain.md`.
- [x] Remove credential-file hashes from the two E2E task records while retaining behavioral evidence.
- [x] Complete the `07-13` task PRD enough to describe its still-planning scope; do not falsely mark it complete.
- [x] Add a RED test proving authoritative idle status overrides pending prompt history.
- [x] Remove the optimistic `TimelineRender::working` state and observe focused GREEN.

## 3. Commit Tooling And Records

- [x] Fetch and require `main` to remain even with `origin/main` before the first commit.
- [x] Commit the local planning ignore rule.
- [x] Verify generated hashes and Trellis CLI smoke paths; commit Trellis/OMP 0.6.7.
- [x] Validate and commit project agent workflow/configuration files.
- [x] Validate and commit architecture/context documentation.
- [x] Validate and commit each sanitized `07-14` E2E task record separately.

## 4. Commit Rust Changes

- [x] Stage the exact dependency-only patch; require 13 files and `+13/-92`.
- [x] Export the staged index and run format, Clippy, workspace tests, and binary builds against that snapshot.
- [x] Commit dependency cleanup and confirm only release/version plus indicator code remains.
- [x] Run focused indicator tests, format, Clippy, workspace tests, and binary builds.
- [x] Confirm Cargo version, root changelog, archived changelog, and binaries all report `0.33.10`.
- [x] Commit the authoritative running-indicator fix and reinstall the corrected user-local binaries.

## 5. Close And Push

- [x] Run Trellis quality review and validate this task.
- [ ] Archive this task without an automatic broad commit; commit only its archive paths.
- [ ] Record and commit a journal update only if finish-work generates one.
- [ ] Re-fetch; stop rather than force if upstream advanced.
- [ ] Push `main` normally.
- [ ] Verify `HEAD == @{upstream}`, inspect pushed commits, and require empty `git status --short`.
