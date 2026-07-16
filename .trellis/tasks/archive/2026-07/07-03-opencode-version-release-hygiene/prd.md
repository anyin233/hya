# Version and release metadata hygiene

## Goal

Align visible project version documentation and add a regression check so README/release metadata cannot drift from Cargo workspace version.

## Requirements

- Fix the visible README workspace version drift found in the OpenCode parity review.
- Add a regression test that compares the hya crate version to root README and root CHANGELOG metadata.
- Bump the project to version `0.33.9` and archive the previous root changelog according to the project release rule.
- Do not change install, runtime, provider, or TUI behavior.

## Acceptance Criteria

- [x] The new regression test failed first because README reported `0.28.9` while the fetched workspace baseline was `0.33.8`.
- [x] README, root `CHANGELOG.md`, root `Cargo.toml`, and every hya package in `Cargo.lock` agree on `0.33.9`.
- [x] Previous root changelog content is archived at `docs/changes/CHANGELOG_0.33.8.md`.
- [x] Targeted tests, the full Rust CI-equivalent gate, and local `hya`/`hya-backend` builds pass.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
