# 0.43.24

## Fixes

- **The installer no longer asks you to add a directory that is already on `PATH`.** `hya-install.sh` and `hya update` compared `PATH` entries with the install prefix as text. When `PATH` reaches `<prefix>/bin` through a symlink, for example `/tmp` and `/private/tmp` on macOS or a symlinked `~/.local`, they printed a needless "Add … to PATH" hint. They now compare real directories.
- **Release builds follow the first-party bundle set.** The release workflow still expected twelve first-party bundles, including the removed `hya/subagents`, so the v0.43.23 build failed its smoke test and was never published. Its release notes are in [docs/changes/CHANGELOG_0.43.23.md](docs/changes/CHANGELOG_0.43.23.md), and this release ships those changes. The workflow now reads one `first_party=(…)` list per step, and `release-rehearsal` rejects any list that differs from `hya_bundle::FIRST_PARTY_BUNDLES`.
- **`hya serve start` no longer fails after a quick stop or restart.** A stopping server released the database lock before its runtime-owner lock, so a server started in that gap failed with `RUNTIME_OWNER_BUSY` and the start gave up. The server now releases the runtime owner first. A start that still meets a held owner exits 75 ("database in use"), so the starter waits and retries.
- **A prompt sent from `/status`, `/models`, `/todos`, `/workflows`, or `/api` shows its reply.** Those pages replaced the transcript until you opened a session, so the reply stayed out of sight. Sending a prompt or `!command` now returns to the transcript.
