# Version and release metadata hygiene

## Goal

Align visible project version documentation and add a regression check so README/release metadata cannot drift from Cargo workspace version.

## Requirements

- Fix the visible README workspace version drift found in the OpenCode parity review.
- Add a regression test that compares the hya crate version to root README and root CHANGELOG metadata.
- Bump the project to version `0.29.3` and archive the previous root changelog according to the project release rule.
- Do not change install, runtime, provider, or TUI behavior.

## Acceptance Criteria

- [ ] A red test fails on the current branch because README reports `0.28.9` while the workspace is `0.29.2`.
- [ ] README active-development version, root `CHANGELOG.md` heading, root `Cargo.toml`, and `Cargo.lock` workspace package versions all agree on `0.29.3`.
- [ ] Previous root `CHANGELOG.md` content is moved to `docs/changes/CHANGELOG_0.29.2.md`.
- [ ] Targeted test and Rust workspace verification pass.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
