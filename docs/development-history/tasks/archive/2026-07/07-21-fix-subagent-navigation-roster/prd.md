# Fix subagent navigation and roster shortcuts

## Goal

Make subagent observation navigation recover reliably and make all subagent controls discoverable next to the roster.

## Background

- Entering a subagent that failed to start can leave the TUI unable to return to the main agent with `Esc`.
- Using a split-pane shortcut while observing a subagent can leave `Esc` ineffective while also failing to create the split.
- The subagent roster and its controls are not currently presented as one consistently docked surface.

## Requirements

- `Esc` must return from every subagent observation state to the main agent, including failed-start and failed-split paths.
- Subagent split shortcuts must either create the requested split or preserve a state from which `Esc` returns to the main agent.
- Every subagent-related shortcut must be displayed below the subagent roster.
- In the main-agent view, the roster must sit directly above the prompt composer and use the same width.
- In a subagent view, where no prompt composer exists, the roster and shortcut surface must dock to the bottom of the interface.
- Preserve the existing borderless, tonal TUI design and deterministic keyboard behavior.
- Add the smallest deterministic regression coverage that exercises the reported failure paths.

## Acceptance Criteria

- [ ] A regression test reproduces the pre-fix inability to leave a failed-start subagent with `Esc`, then passes after the fix.
- [ ] A regression test reproduces the pre-fix split-shortcut state where no split appears and `Esc` cannot return, then passes after the fix.
- [ ] From each subagent observation state, pressing `Esc` restores the main-agent view.
- [ ] Valid split shortcuts still produce the intended observation layout.
- [ ] In the main-agent view, the rendered roster is immediately above the prompt composer and aligned to its horizontal bounds.
- [ ] In a subagent view, the same roster and shortcut surface is bottom-docked without introducing a prompt composer.
- [ ] All supported subagent shortcuts are visibly rendered below the roster without overflow at covered terminal widths.
- [ ] Focused TUI tests and the repository verification gate pass.

## Out Of Scope

- Redesigning subagent lifecycle, spawning, or backend event contracts unless root-cause evidence requires it.
- Adding new subagent shortcuts or a new TUI component framework.

## Notes

- This is a complex task and requires `design.md` and `implement.md` before activation.
