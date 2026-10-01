# 0.45.0

## Install and update

- **One installer for both sides.** `curl -fsSL https://hya.ed-aisys.com/install.sh | sh`
  now finds the newest `backend/<version>` and `frontend/<version>` releases on
  GitHub and installs both into `~/.local`. Pass `--backend-only` or
  `--tui-only` to install one side; `--version`, `--prefix`, and `--force` work
  as before and apply to each selected side. Each side is checked against its
  release's `SHA256SUMS` and rolled back on its own if it fails.
- **`hya update` updates the backend and the frontend.** Use
  `hya update --backend-only` or `hya update --tui-only` to update one side.
- **Breaking:** `hya update tui` is removed; use `hya update --tui-only`. The
  missing-frontend and missing-Bun messages of bare `hya` now name that command.
- **Releases no longer carry installer scripts.** `hya-install.sh` and
  `hya-tui-install.sh` are not release assets; `hya.ed-aisys.com/install.sh`
  serves `scripts/hya-install.sh` from `main`. Each release body shows the
  pinned command, for example
  `curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --backend-only --version 0.45.0`.

The previous 0.44.0 notes are archived in `docs/changes/CHANGELOG_BACKEND_0.44.0.md`.
