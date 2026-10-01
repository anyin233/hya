# 0.43.43

## Distribution

- Backend releases are published independently under `backend/<version>` with a
  backend-only archive and installer. Frontend assets are installed separately
  with `hya update tui`.

## Fixes

- Backend-only installs no longer require frontend assets for server and other
  non-interactive commands; commands that start the TUI explain how to install
  the frontend.
