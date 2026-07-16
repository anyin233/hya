# TUI theme picker parity slice

## Goal

Add a bounded /themes TUI flow with named built-in themes and controller/render tests, reducing the OpenCode theme-picker gap without broad UI rewrites.

## Requirements

- Add a bounded TUI theme picker slice that reduces the OpenCode theme-picker parity gap without broad redesign.
- Provide named built-in themes and a `/themes` slash command.
- Preserve terminal-first design rules from `DESIGN.md`: semantic theme fields, no decorative colors, no raw `Color::Rgb` in render code outside theme definitions.
- Keep rendering pure; no filesystem config persistence in this PR.

## Acceptance Criteria

- [x] A red command-routing test proved `/theme` and `/themes` were not recognized.
- [x] `/themes` opens a dialog listing named themes, marks the current theme, and preselects it.
- [x] Selecting a theme updates `AppState` immediately and subsequent draw calls use it.
- [x] Built-in command help/completion include `/theme` and `/themes`.
- [x] Version `0.33.11` metadata, full Rust CI-equivalent checks, and local binary builds are complete.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
