# Drop legacy TUI — Design

## Context

Current runtime facts:

- `hya-backend` depends on `hya-legacy-tui` and owns the legacy `--mini` controller/render path.
- Bare `hya-backend` already routes to `serve::cmd_tui_hya`, starts an HTTP/SSE backend, and launches the current `hya` frontend.
- `hya-tui` / `hya-tui-lib` are separate current frontend crates and are not the backend legacy path.
- The old `--resume` option is only consumed by the legacy path; the current frontend has `AppEvent::LoadSession` and `load_session()`, but that path navigates before validating and always posts “Session loaded”.

ADR-0005 records the decision to delete the legacy TUI surface, remove `--mini`, and preserve only Resume.

## Boundaries

- Backend CLI boundary: `crates/hya-backend/src/cli_args.rs`, `main.rs`, `serve.rs`, and tests.
- Frontend CLI boundary: `crates/hya/src/args.rs`, `main.rs`, transport startup, and tests.
- Frontend runtime boundary: `crates/hya-tui/src/app/runtime.rs` and harness/render tests.
- Workspace/package boundary: root `Cargo.toml`, deleted `crates/hya-legacy-tui/`, docs, changelog.

## Data flow

Valid startup resume:

```text
hya-backend --db <store> --resume <session>
  -> cmd_tui_hya(model, db, yolo, resume)
  -> launch_hya(base_url, resume)
  -> hya --server <base_url> --resume <session>
  -> Transport::connect(...)
  -> PendingClient slot set
  -> session_get(<session>)
  -> AppEvent::LoadSession(<session>)
  -> load_session() backfills and renders transcript
```

Invalid startup resume:

```text
hya --resume <session>
  -> connect to selected runtime
  -> session_get(<session>) fails
  -> AppEvent::Toast("resume failed: ...")
  -> stay on Home; no LoadSession
```

## Contracts

- `--mini` has no compatibility shim. Removing the Clap field makes it an unknown argument.
- `--resume` belongs to interactive startup only. Backend validation rejects it with any subcommand.
- `serve::cmd_tui_hya` accepts an optional resume id and forwards it only to the launched current frontend.
- Frontend startup resume is a validation path separate from `load_session()`. `load_session()` remains for in-app navigation; startup resume validates first because CLI resume must not land on an empty session view.
- The frontend may allow standalone/in-memory `hya --resume`; if the session is not in that connected runtime, the visible failure is the correct result.
- No `/tui/select-session` compat endpoint is used for startup resume; it queues compat control payloads rather than native `LoadSession` state.

## Testing seams

Confirmed seams from grilling/TDD:

1. Backend CLI parser and validation.
2. Backend current-frontend launch argument construction.
3. Frontend CLI parser.
4. Frontend startup resume behavior through public runtime/client events.
5. Headless rendered TUI frame after resume.
6. Terminal/tmux smoke path for real startup behavior.

## Release bookkeeping

- Bump `[workspace.package].version` to `0.32.0`.
- Move current root changelog to `docs/changes/CHANGELOG_0.31.0.md`.
- Replace root changelog with `# 0.32.0` notes for the legacy TUI removal and Resume migration.

## Rollback

Rollback requires restoring the deleted crate, backend dependency, `--mini` parser/controller path, docs references, and changelog/version bump. Because the crate deletion is large and public, ADR-0005 documents the decision to prevent accidental reintroduction.
