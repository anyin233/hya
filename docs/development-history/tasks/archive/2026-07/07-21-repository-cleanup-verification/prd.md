# Clean repository and verify outstanding work

## Goal

Preserve and publish all legitimate outstanding repository records, prove that
their represented implementation is complete or intentionally deferred, and
leave local `main` clean and synchronized with `origin/main`.

## Background

- Before this task, `main` matched `origin/main` at `eec30372` and only four
  dirty groups existed: Session 9 workspace records, one completed archived
  reasoning-default task, and one foreign untracked IR task directory.
- The reasoning-default task changed only `~/.config/hya/config.yaml`. A fresh,
  sanitized check confirmed both installed binaries are `0.33.14`, all eight
  configured models retain their exact order, variants, and maximum defaults,
  `12th-oai` uses `openai-response`, and `hya-backend models` loads all IDs.
- The untracked IR conformance task does not belong to this project. The user
  explicitly confirmed that removing the entire directory is safe.

## Requirements

- Preserve the completed reasoning-default task archive and its Session 9
  journal/index record.
- Remove `.trellis/tasks/07-17-ir-compiler-stack-conformance/` completely; do
  not publish its foreign planning records in this repository.
- Complete and archive this cleanup task after verification.
- Stage only exact approved paths and review every staged diff before commit.
- Do not commit credentials or raw user-configuration values.
- Do not change product source, `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`,
  release metadata, or the installed user configuration.
- Use normal atomic commits and a non-force push to `origin/main`.
- Finish with an empty worktree and local `HEAD` equal to fetched
  `origin/main`.

## Acceptance Criteria

- [x] The installed configuration passes the sanitized eight-model assertion
  and `hya-backend models` returns the expected eight qualified IDs.
- [x] The completed reasoning-default task and Session 9 record are tracked.
- [x] `.trellis/tasks/07-17-ir-compiler-stack-conformance/` no longer exists and
  no commit contains its untracked contents.
- [x] This cleanup task is completed and archived with its session record.
- [x] Every new commit contains only its declared Trellis paths and passes
  whitespace and privacy checks.
- [x] No product, version, changelog, release, or home-directory file appears
  in the baseline-to-HEAD diff.
- [ ] After a final fetch and push, the branch is `main`, `git status --short`
  emits no output, and `HEAD` equals `origin/main`.

## Out Of Scope

- Executing, preserving, or publishing the foreign IR compiler-stack audit.
- Product implementation, tests unrelated to changed files, version bumps, or
  release publication.
- Tracking or deleting ignored `.planning/` session records.
- Changing Trellis archive-helper behavior; that is a separate follow-up if
  broad staging remains a recurring issue.
