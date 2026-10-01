# 0.44.1

## Performance

- Avoid a full catalog refresh when `/new` creates a session; use the returned session projection and open it directly.

## Distribution

- Frontend releases are independently published under `frontend/<version>` tags
  as `hya-frontend-<version>-<target>.tar.gz` archives.
- The frontend archive contains Bun, the TUI, and the WebUI, but never installs
  `bin/hya`; use the backend release for the command and headless server.
- `hya update tui` installs or updates the frontend beside an existing backend.
- This frontend remains compatible with backend `0.43.41` and newer.
