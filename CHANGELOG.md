# 0.28.7

- Fixed skill discovery to search project `.hya/skills` before user and agent-runtime skill directories, keeping the first duplicate found.
- Added session-workdir skill indexes for model prompts, skill loading, OpenCode skill/command metadata, and skill-backed slash command surfaces.
- Fixed OpenCode session/init agent catalogs, reference guidance, and external reference permissions to use the active session workdir.
