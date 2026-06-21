# Ratatui TUI redesign

## Goal

Upgrade yaca's default terminal UI into a polished Rust/ratatui developer workspace inspired by opencode's TUI visual language, without copying opencode functionality or implementation technology.

## Requirements

- Render the TUI with Rust and `ratatui` only; do not introduce OpenTUI, Solid, TypeScript, JavaScript renderers, or plugin slot systems.
- Keep `yaca-tui` as a pure rendering/view crate. Terminal raw mode, crossterm input, async streaming, and cancellation remain owned by `yaca-cli`.
- Preserve existing interactive behavior: prompt input, submit on Enter, quit with Ctrl-C/Ctrl-D/Esc, PgUp/PgDn and Up/Down scrolling, streaming state, permission takeover, goal/loop/team status.
- Replace the current large bordered conversation box with a time-line style layout: subtle background, user panels with left rails, assistant text blocks, compact tool rows, and minimal metadata.
- Add responsive context treatment: wide terminals show a right sidebar for session/model/goal/loop/team/permission summaries; narrow terminals keep content readable and avoid overlap.
- Add a theme layer with named semantic colors for background, panels, borders, text, accents, success, warning, error, and info.
- Keep the implementation focused on UI presentation. Do not add opencode commands, sharing, fork, plugin, LSP, MCP, external editor, provider setup, or theme marketplace features.

## Acceptance Criteria

- [ ] The empty state, chat transcript, prompt, running state, permission request, goal/loop indicators, and team panel still render.
- [ ] `ratatui::backend::TestBackend` tests cover at least 50x18, 80x24, and 120x36 terminal sizes.
- [ ] A wide render includes a right sidebar summary; a narrow render does not overlap prompt, transcript, or status text.
- [ ] User messages render as panel-like blocks with a visible left rail; assistant messages render as lighter indented text; tool parts render with status-specific labels.
- [ ] No non-Rust TUI framework or JavaScript rendering dependency is added.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` pass after implementation.

## Notes

- Visual reference is limited to opencode's TUI style: dark surface hierarchy, sparse borders, compact footer/status, message rails, bottom prompt, and responsive side context.
- Existing yaca event projection remains the source of truth; this task is a rendering and view-model upgrade, not an agent-engine change.
