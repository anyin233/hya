# TUI theme picker parity slice

## Goal

Add a bounded /themes TUI flow with named built-in themes and controller/render tests, reducing the OpenCode theme-picker gap without broad UI rewrites.

## Requirements

- Add a bounded TUI theme picker slice that reduces the OpenCode theme-picker parity gap without broad redesign.
- Provide named built-in themes and a `/themes` slash command.
- Preserve terminal-first design rules from `DESIGN.md`: semantic theme fields, no decorative colors, no raw `Color::Rgb` in render code outside theme definitions.
- Keep rendering pure; no filesystem config persistence in this PR.

## Acceptance Criteria

- [ ] A red controller test proves `/themes` is not currently recognized.
- [ ] `/themes` opens a dialog listing named themes and marks the current theme.
- [ ] Selecting a theme updates `AppState` and subsequent `draw` calls use that theme.
- [ ] Help/completion include `/themes`.
- [ ] Assigned version `0.29.5` release metadata is updated.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
