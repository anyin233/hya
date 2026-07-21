# Implementation Plan: Repository Cleanup Verification

## Gate

- [x] User reviews and approves the PRD, design, and this plan.
- [x] Activate this task only after approval and confirm status `in_progress`.

## 1. Preflight And Content Audit

- [x] Fetch `origin` without merging and require branch `main` with no remote
  divergence from the recorded baseline.
- [x] Require the dirty inventory to contain only the completed prior work
  record, the foreign untracked directory, and this cleanup task; stop on any
  new path.
- [x] Validate every task JSON/JSONL file and scan all proposed content for
  credentials or copied private configuration.
- [x] Require the foreign IR path to resolve exactly inside this repository,
  contain no tracked files, and appear only as untracked content.

## 2. Revalidate Completed Configuration Work

- [x] Require installed `hya` and `hya-backend` version `0.33.14`.
- [x] Rerun the sanitized YAML assertion for all eight model IDs, order,
  variants, defaults, and the `12th-oai` Responses kind.
- [x] Require `hya-backend models` to return exactly the expected eight IDs.

## 3. Remove Foreign Residue And Preserve Prior Records

- [x] Delete only `.trellis/tasks/07-17-ir-compiler-stack-conformance/` after
  its exact path and untracked-only preconditions pass; confirm it is absent.
- [x] Stage only the completed reasoning-default archive and Session 9
  workspace files; audit the cached patch, then commit atomically.
- [x] Confirm the foreign path is absent from the commit and the remaining dirty
  paths contain only this cleanup task.

## 4. Close This Cleanup Task

- [x] Record verification and commit evidence in this task.
- [x] Run the Trellis quality review and record that no durable spec update is
  required.
- [x] Add Session 10, archive this task with `--no-commit`, and confirm only the
  expected task move and workspace records changed.
- [x] Stage only the cleanup archive and Session 10 workspace files, inspect the
  complete cached diff, and commit atomically.

## 5. Publish And Prove Clean State

- [x] Review the baseline-to-HEAD names and commits; require only approved
  Trellis paths and no secrets.
- [ ] Require exactly two cleanup commits and prove the foreign path never
  entered Git history.
- [ ] Fetch immediately before a normal push; stop on divergence.
- [ ] Push `main`, fetch again, and require empty `git status --short`, branch
  `main`, and identical `HEAD` and `origin/main` hashes.
- [ ] Perform no repository write after the final clean-state proof.
