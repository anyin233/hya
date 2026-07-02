# 0.29.0

- Fixed Compat command execution to expand skill-backed slash command templates when clients post `/session/:id/command` or `/api/session/:id/command` without pre-expanded text.
- Preserved native Compat command fallback behavior while using each session's effective workdir for skill/custom command lookup.
- Fixed multi-digit slash command placeholders without re-expanding replacement arguments.
- Fixed `/project/git/init` to initialize nested project directories and stale `.git` markers instead of treating an outer or invalid parent repository as the target project repo.
- Added `hya-tui-lib`, a reusable ratatui component/layout library with geometry, color, flex layout, overlay, layer validation, declarative component, and ratatui adapter primitives.
- Migrated `hya-tui` reusable geometry, layout, overlay, and draw-adapter paths to compatibility re-exports backed by `hya-tui-lib`.
