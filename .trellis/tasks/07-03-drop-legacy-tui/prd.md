# Drop legacy TUI

## Goal

Remove the legacy TUI surface from the workspace while preserving the interactive **Resume** contract on the current frontend.

## Requirements

- Delete the `hya-legacy-tui` crate, not just the backend entrypoint. Cargo uses `members = ["crates/*"]`, so keeping the directory keeps the crate alive.
- Remove the backend legacy `--mini` path and its controller/render modules.
- Keep the default interactive path unchanged: bare `hya-backend` launches the current `hya` frontend through `serve::cmd_tui_hya`.
- Remove `--mini` as a recognized option; `hya-backend --mini` should fail as an unknown argument.
- Preserve Resume on the current frontend:
  - `hya --resume <session>` is accepted.
  - `hya-backend --resume <session>` is accepted only for interactive startup and forwarded to the launched `hya --server ... --resume <session>`.
  - `--resume` with non-interactive subcommands is rejected.
  - Startup resume validates the session through the connected runtime before navigating.
  - If the session is unavailable in the connected runtime, the frontend stays on Home and reports a visible failure.
- Do not port other `--mini`-only behaviors; the current frontend is the source of truth.
- Update all docs and archived references that point at the deleted crate/path.
- Update release bookkeeping: workspace version `0.32.0`; root `CHANGELOG.md` for `0.32.0`; prior root changelog moved to `docs/changes/CHANGELOG_0.31.0.md`.

## Acceptance Criteria

- [ ] `crates/hya-legacy-tui/` is gone and no workspace dependency points to it.
- [ ] `crates/hya-backend` no longer compiles or links a legacy TUI controller/render path.
- [ ] `hya-backend --mini` is an unknown argument.
- [ ] `hya-backend --resume <session>` with no subcommand forwards the session id to the current frontend launch.
- [ ] `hya-backend --resume <session> exec ...` and equivalent non-interactive use reject `--resume`.
- [ ] `hya --resume <session>` validates with `session_get` before sending `AppEvent::LoadSession` or navigating.
- [ ] Invalid/unavailable startup resume leaves the frontend on Home and shows a visible error.
- [ ] Headless render coverage proves a valid startup resume shows the resumed transcript/session.
- [ ] Terminal/tmux QA captures a valid resumed session and an invalid resume failure.
- [ ] Live docs, archived docs, and Trellis references no longer point at `hya-legacy-tui` or `--mini` as supported behavior.
- [ ] `Cargo.toml`, root `CHANGELOG.md`, and `docs/changes/CHANGELOG_0.31.0.md` reflect the `0.32.0` release bookkeeping.
- [ ] Required Rust gates pass for touched areas: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo build -p hya`.
