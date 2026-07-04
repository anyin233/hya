# Drop legacy TUI — Implementation Plan

## Rules

- TDD loop: one failing test, minimal implementation, focused pass, then next slice.
- Tests target public seams; do not test private helpers unless they are the only stable seam.
- Use the existing current frontend; do not port other `--mini` behavior.
- Keep changes boring: delete legacy code rather than shimming it.

## Phase 1 — Backend CLI cutover

1. Add failing backend CLI tests:
   - help no longer contains `--mini`.
   - parsing `--mini` fails as unknown.
   - `--resume` with a subcommand is rejected.
2. Implement the minimal parser/validation changes:
   - remove `mini` field.
   - validate `resume` only with interactive startup.
   - dispatch interactive startup through `serve::cmd_tui_hya(..., resume)`.
3. Add failing launch-arg test for forwarding resume to current frontend.
4. Implement launch forwarding in `serve.rs` without launching a real process in the test seam.
5. Run focused backend tests for `cli_args`/`serve`.

## Phase 2 — Current frontend Resume

1. Add failing frontend CLI parser tests for `hya --resume <session>` and missing argument.
2. Implement `Args.resume` parsing/help text.
3. Add failing runtime/startup resume test:
   - when `session_get` succeeds, startup sends `LoadSession` / renders the session.
   - when `session_get` fails, startup sends a visible toast and does not navigate.
4. Implement resume validation after the client slot is set/backend is ready.
5. Add headless render coverage with `TestBackend` proving resumed transcript/session text is visible.
6. Run focused frontend tests.

## Phase 3 — Delete legacy surface

1. Remove `mod tui` usage and backend legacy TUI modules/tests.
2. Delete `crates/hya-legacy-tui/`.
3. Remove `hya-legacy-tui` workspace dependency and backend dependency.
4. Remove or update all references to `hya-legacy-tui` and supported `--mini` across docs, archived docs, Trellis specs/tasks, and comparison docs.
5. Run `cargo check -p hya-backend -p hya -p hya-tui` or tighter equivalent to catch dangling imports.

## Phase 4 — Release bookkeeping

1. Move current `CHANGELOG.md` to `docs/changes/CHANGELOG_0.31.0.md`.
2. Bump `[workspace.package].version` in root `Cargo.toml` to `0.32.0`.
3. Write new root `CHANGELOG.md` headed `# 0.32.0` with this public surface removal.
4. Verify changelog/version alignment by reading the files.

## Phase 5 — Verification and QA

1. Run focused tests added in Phases 1–2.
2. Run required Rust gates:
   - `cargo fmt --all --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`
   - `cargo build -p hya`
3. Run terminal/tmux QA:
   - create or reuse a persistent DB with a known session.
   - launch `hya-backend --db <db> --resume <session>` in tmux; capture resumed transcript.
   - launch with a missing session; capture visible failure and Home route.
4. Commit and push the atomic feature change only after tests and verification pass.
