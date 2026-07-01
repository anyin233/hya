# 0.28.10

- Fixed OpenCode command execution to expand skill-backed slash command templates when clients post `/session/:id/command` or `/api/session/:id/command` without pre-expanded text.
- Preserved native OpenCode command fallback behavior while using each session's effective workdir for skill/custom command lookup.
- Fixed multi-digit slash command placeholders without re-expanding replacement arguments.
- Fixed `/project/git/init` to initialize nested project directories and stale `.git` markers instead of treating an outer or invalid parent repository as the target project repo.
