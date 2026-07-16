# 0.32.1

Agent catalog refresh release: replaces local project-discovered agents with hya-specific Agent definitions.

- **Added hya Agent catalog.** Added `hya-main` and seven transient specialist subagents under the project `.hya/agents` catalog for hya discovery.
- **Set hya-main as the project default.** The tracked `opencode.json` now resolves new project sessions without an explicit Agent to `hya-main`.
- **Clarified Agent catalog language.** Added glossary language distinguishing the disk/config Agent catalog from the live team Roster.
