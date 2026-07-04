# 0.32.0

Legacy TUI removal release: deletes the old backend-owned TUI crate and makes the current `hya`/`hya-tui` frontend the only interactive surface.

- **Removed legacy TUI.** Deleted `crates/hya-legacy-tui` and the backend legacy controller/render path. Bare `hya-backend` still starts the HTTP/SSE backend and launches the current `hya` frontend.
- **Removed `--mini`.** `hya-backend --mini` is now an unknown argument instead of a compatibility alias.
- **Preserved interactive Resume.** `hya --resume <session>` validates the session through the connected runtime before navigating; `hya-backend --resume <session>` forwards the id to the launched current frontend for interactive startup only.
- **Updated docs and release bookkeeping.** Current TUI docs, compatibility notes, archived plans, Trellis references, and changelog history now describe the legacy surface as removed or superseded.
