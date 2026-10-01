# 0.44.0

## Distribution

- Backend and frontend are now independently published under `backend/<version>`
  and `frontend/<version>` tags.
- Backend archives contain `bin/hya`, backend bundles, and the Bun adapter, but
  no TUI or WebUI. Frontend assets are installed separately with `hya update tui`.
- Backend-only installations support headless commands without frontend assets;
  interactive startup reports the explicit frontend install command when needed.
