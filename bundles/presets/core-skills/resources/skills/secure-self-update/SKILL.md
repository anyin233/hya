---
name: secure-self-update
description: >-
  Use when installing or updating a hya release with `hya update`: the embedded release installer, `--backend-only`/`--tui-only`, pinned versions, `SHA256SUMS` verification and per-side rollback, plus the source-checkout rebuild-and-restart flow. Do not use for bundle install or plugin load.
---
<!-- Trusted core Skill; its metadata and body are prepared from this bundle. -->

# Update hya

Use this skill when installing or updating an independent hya release with
`hya update`. Do **not** use it for bundle install or plugin load.

## What `hya update` does

`hya update` pipes the installer embedded from `scripts/hya-install.sh` to `sh`
for the running hya's prefix. That is exactly the script
`https://hya.ed-aisys.com/install.sh` serves: it downloads each selected side's
release archive, verifies it against that release's `SHA256SUMS`, and rolls the
side back when it fails.

- Bare `hya update` updates the backend and the frontend.
- `--backend-only` installs/updates `bin/hya`, the bundles, and the Bun adapter.
- `--tui-only` installs/updates Bun, the TUI, and the WebUI.
- `--version <v>` pins each selected side, `--force` reinstalls the current
  versions, and `--prefix <dir>` targets a prefix other than the running hya's.
- `hya update version` prints the updater's package version.

## Hard rules

- `hya update` refuses a build-tree `hya`: only an installed `<prefix>/bin/hya`
  beside `<prefix>/lib/hya` can update itself.
- Never bypass the installer's `SHA256SUMS` verification by copying archives by
  hand.
- `install.sh` at the repository root remains break-glass bootstrap/manual
  recovery.

## References

- Guide: `docs/install.md`
- Script: `scripts/hya-install.sh`
- Crate: `crates/hya-updater`

## Source-checkout flow (rebuild and restart)

To replace the running backend with code you changed in a hya source checkout
(no release involved):

1. Build: `cargo build -p hya-backend --bin hya`.
2. Restart from the new build with the tests that prove the change:
   `./target/debug/hya serve restart --verify 'cargo test -p <crate>'`.
   The restart first runs `hya serve check` of the new build against a snapshot
   of the live database; a failing check or verify command leaves the running
   backend untouched — fix and retry.
3. The current turn resumes in the new build; the next turn runs the new code.
4. `hya serve status`: a `restart` line means the new build failed to start and
   the backend rolled back to the previous build; read `<db>.server.log`.

## Agent boundaries

Do not waive the installer's checksum verification, copy archives in by hand, or
claim an update happened without the installer's own output.
