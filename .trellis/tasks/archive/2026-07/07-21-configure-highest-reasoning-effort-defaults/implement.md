# Implementation Plan: Highest Reasoning Effort Defaults

## Gate

- [x] User reviews and approves `prd.md`, `design.md`, and this plan.
- [x] Run `task.py start` only after that approval; confirm task status is
  `in_progress` before editing the user configuration.

## 1. Establish Baseline And RED

- [x] Confirm the resolved `hya` and `hya-backend` binaries both report
  `0.33.14`.
- [x] Confirm the live config still has the recorded provider IDs, model IDs,
  ordering, `12th-oai.kind: openai`, and shorthand model entries. Stop on drift.
- [x] Create a private temporary directory under `/tmp/opencode`, copy the
  config with metadata preserved, and record its SHA-256 and mode without
  printing credentials.
- [x] Run `hya-backend models`; record the exact eight qualified model IDs as
  the baseline catalog.
- [x] Run a Ruby standard-library YAML assertion for the complete target route,
  model order, variants, and defaults. Require the expected RED caused only by
  the current Chat kind and shorthand entries. A parser or tooling failure is
  not a valid RED.

## 2. Apply The Configuration Change

- [x] Use one narrow patch on `/home/yanweiye/.config/hya/config.yaml`.
- [x] Change only `12th-oai.kind` from `openai` to `openai-response`.
- [x] Replace each of the eight model strings with the detailed entry specified
  in `research/reasoning-effort-matrix.md`, preserving provider and model order.

## 3. Verify GREEN And Runtime Loading

- [x] Rerun the identical Ruby/YAML target assertion and require GREEN.
- [x] Parse the backup and edited files, derive the expected target by applying
  only the approved provider-kind and model-list mutations to the baseline, and
  require exact equality with the edited object.
- [x] Run installed `hya-backend models`; require successful config loading and
  exactly the original eight qualified model IDs with no duplicates.
- [x] Inspect the backup-to-current diff and confirm it contains only the route
  change and eight model expansions.
- [x] Review `git status` and leave all unrelated workspace changes untouched.

No workspace build, release bump, changelog edit, or repeated network probe is
required for this user-configuration-only change.

## 4. Rollback Or Complete

- [x] Rollback was not required because every post-edit gate passed.
- [x] After every check passes, remove only the temporary backup created by this
  task and record the checks in the task progress.
- [x] Confirm no commit or push was requested.
