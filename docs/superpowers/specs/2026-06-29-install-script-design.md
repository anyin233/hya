# Install Script Design

## Problem

A source checkout needs one command that installs a complete hya onto the user's system PATH. Complete means both shipped binaries are available:

- `hya` from `crates/hya`, the user-facing TUI/frontend that starts the real hya backend in-process by default.
- `hya-backend` from `crates/hya-backend`, the backend CLI/API binary used for login, headless execution, and server modes; `login` and `models` are defined in `crates/hya-backend/src/cli_args.rs`.

The installer must not alias `hya` to `hya-backend`; that would bypass the frontend/native transport path in `crates/hya/src/transport.rs`.

## Chosen Approach

Add a root `install.sh` Bash script that builds and installs both binaries from the current checkout.

Supported options:

- `--prefix DIR`: install to `DIR/bin`; defaults to `/usr/local`.
- `--bin-dir DIR`: install directly to `DIR`; overrides `--prefix`.
- `--profile release|dev|debug`: default `release`; `dev` and `debug` both use plain `cargo build` and `target/debug`.
- `--dry-run`: print build, install, verification, and API setup steps without writing.
- The file is executable so users can run `./install.sh` directly.
- `--help`: print usage.

Build behavior:

- Release: `cargo build --locked --profile release -p hya -p hya-backend --bins`, artifacts under `target/release`.
- Dev/debug: `cargo build --locked -p hya -p hya-backend --bins`, artifacts under `target/debug`.
- Never call `cargo build --profile debug`.

Install behavior:

- Normalize `bin_dir` to an absolute path before PATH verification so relative `--bin-dir bin` compares against the same path shape as `command -v hya`.
- Before building, preflight target permissions; if the target bin directory or nearest existing parent is not writable, fail early with `rerun with sudo ./install.sh` or `use --bin-dir ~/.local/bin` guidance.
- Create the target bin directory if needed.
- Copy `hya` and `hya-backend` to temp files in the target bin dir, back up existing installed binaries before replacement, then rename new binaries into place.
- Trap restores backups and removes temp/partial files on failure until installed-path and PATH verification pass.
- Verify the installed paths directly: `<bin-dir>/hya --version` and `<bin-dir>/hya-backend --help`.
- Verify PATH discovery exactly: `command -v hya` must resolve to `<bin-dir>/hya`; otherwise fail with the exact `export PATH="<bin-dir>:$PATH"` fix.

## API Setup Guidance

After install, print concise first-run guidance:

- hya works offline by default through the development provider.
- Live model config path is `$XDG_CONFIG_HOME/hya/config.yaml`, or `~/.config/hya/config.yaml` when `XDG_CONFIG_HOME` is unset.
- Include a minimal Anthropic provider YAML sample using `{env:ANTHROPIC_API_KEY}`.
- Show `hya-backend login anthropic "$ANTHROPIC_API_KEY"`, `hya-backend models`, then `hya`.

## Version and Changelog

This feature is a user-visible install improvement, so bump workspace version from current `0.28.3` to `0.28.4` in `[workspace.package].version`.

Move current root `CHANGELOG.md` content to `docs/changes/CHANGELOG_0.28.3.md`, then replace root `CHANGELOG.md` with only `# 0.28.4` and the install-script release note.

## Tests

Add `tests/install_script.sh` before production script code. It runs the installer in dry-run mode and asserts:

- Help documents `--prefix`, `--bin-dir`, `--profile`, and `--dry-run`.
- Debug profile dry-run prints plain `cargo build --locked -p hya -p hya-backend --bins` and does not print `--profile debug`.
- Dry-run mentions both install targets, `hya` and `hya-backend`.
- Dry-run includes config path and login guidance.

This avoids writing to `/usr/local` during tests while still pinning the behavior that matters.
