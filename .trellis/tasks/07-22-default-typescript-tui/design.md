# Design: TypeScript TUI as the Default

## Chosen Architecture

```text
hya (stdlib Unix exec shim)
  -> resolve adjacent hya-ts from current_exe
  -> exec hya-ts with argv[0] = hya and all original arguments

hya-ts (existing launcher)
  -> import/auth command, or
  -> attach to --server, or own hya-backend
  -> spawn Bun with the prepared hya-tui-ts runtime

bare hya-backend
  -> ephemeral loopback server
  -> hya --server <url> [--session <id>]
  -> same adjacent hya-ts launcher in attached mode
```

`hya` does not supervise or spawn another launcher process. On Unix it replaces
itself with the adjacent `hya-ts` executable using the standard-library process
extension. This preserves the PID and leaves signals, process-group ownership,
exit propagation, backend cleanup, and terminal restoration in the existing,
tested `hya-ts` supervisor.

The shim resolves only `current_exe().with_file_name("hya-ts")`. It does not
search `PATH`, add an override, or fall back to Rust. The installer and release
archive already place both executables together as one atomic unit. A missing
sibling is therefore an explicit broken-install error, not a reason to select a
different frontend.

Shared-library extraction is rejected unless an executable-layout test proves
the shim cannot satisfy the supported install. Extracting the private launcher
would move roughly 274 lines without changing behavior and would create a
larger lifecycle surface to review.

## CLI Contract

The existing `hya-ts` Clap definition becomes the shared command contract.
Invocation identity comes from `argv[0]`:

- the shim sets `argv[0]` to `hya`, so help, version, and errors use canonical
  `hya` branding;
- direct `hya-ts` keeps `hya-ts` branding and all existing options;
- `--backend-bin`, `--bun`, auth commands, project/session/model options, and
  their current validation remain supported;
- old Rust-frontend-only options disappear with the Rust frontend; direct resume
  uses the existing TypeScript `--session` spelling.

`--import compat` moves into the shared launcher CLI and short-circuits before
project, runtime, backend, or Bun startup. It reuses
`hya_app::config::import_compat_models_into_config` and the existing config-path
helpers. No new importer or compatibility layer is introduced.

The Bun spawn error gains context naming the attempted executable. Missing Bun
must fail nonzero and must not trigger another frontend.

## Backend Entry

`hya-backend` keeps ownership of its ephemeral server and continues to launch
the configurable `HYA_FRONTEND_BIN`, whose default remains `hya`. Its generated
frontend arguments change only from `--resume <id>` to `--session <id>`.

Because the launched `hya` receives `--server`, `hya-ts` enters attached mode
and does not spawn another backend. This prevents recursion while preserving the
backend's existing server lifetime.

## Source And Test Ownership

`crates/hya/src/main.rs` becomes the shim. The obsolete executable-only modules
`args.rs`, `backend.rs`, `events.rs`, and `transport.rs` are deleted. The package
uses no normal dependency beyond the standard library.

`crates/hya/tests/frontend_cli.rs` is repurposed to prove delegation, canonical
branding, explicit sibling failure, argument and exit-status propagation, and
the migrated import command. `crates/hya-ts/tests/process.rs` retains lifecycle
coverage and adds only missing import and Bun-error contracts.

`crates/hya/tests/native_round_trip.rs` stays in place with its dependencies
moved to dev-dependencies. It validates native transport independently, and
`scripts/verify-no-http.sh` invokes that exact package/test path. Moving it would
add churn without helping this migration. `version_metadata.rs` also remains.

The deprecated `hya-tui` and `hya-tui-lib` crates keep their source and tests;
they no longer have a shipped executable consumer.

## Installation And Release

The existing layout remains unchanged:

```text
<prefix>/bin/hya
<prefix>/bin/hya-ts
<prefix>/bin/hya-backend
<prefix>/lib/hya/hya-tui-ts/...
```

Installer and release smoke checks add a noninteractive attached launch through
`hya` with a no-op Bun executable. This proves the shim finds `hya-ts`, the
launcher finds the prepared runtime, and Bun command construction is reached.
The existing archive-content and all-or-nothing rollback checks remain.

Standalone `cargo install --path crates/hya` is no longer documented or
supported because it cannot provide the adjacent `hya-ts`, backend, or prepared
runtime. `install.sh` is the source-install path.

## Version And Documentation

The live baseline is currently `0.33.28`, so the planned feature version is
`0.33.29`. Before editing metadata, implementation must re-read the live
baseline because another task advanced it during planning. The implementation
then updates workspace, lockfile, TypeScript package, README metadata, archives
the previous root changelog under `docs/changes/`, and writes only the new
version's notes to root `CHANGELOG.md`.

Active README, getting-started, CLI, configuration, architecture, installer,
CI, release, and project-agent documentation must identify TypeScript as the
default. Historical changelogs and archived task records remain unchanged.

## Planner Merge

All four planners selected the exec shim. Their disagreements were resolved as
follows:

- Exec versus extraction: use exec because lifecycle behavior already exists
  and is tested in one place.
- Adjacent-only versus PATH/override fallback: use adjacent-only resolution
  because supported packages are atomic and fallback would hide broken installs.
- Shared versus fixed branding: derive one of the two known invocation names
  from `argv[0]`, preserving both public entrypoints without duplicate parsers.
- Retain versus move the native round-trip test: retain its current path because
  a repository verification script depends on it and dev-dependencies suffice.
- Suggested removal of `--backend-bin` and bootstrap behavior changes: reject
  both as unsupported scope expansion; existing TypeScript capabilities remain.

## Risks And Rollback

- Partial installation: fail with the adjacent path in the diagnostic; installer
  and release smokes block incomplete packages.
- CLI branding drift: process tests execute both names and inspect help/errors.
- Backend resume regression: a focused unit test pins `--session` generation.
- Import side effects: isolated process coverage proves import exits before
  backend or Bun startup and reuses existing importer tests.
- Platform scope: the shim is Unix-only, matching the existing unguarded Unix
  process APIs in `hya-ts`; stop and redesign if supported targets change.
- Concurrent release metadata: re-read before edits and never overwrite another
  task's changelog/version work.

There is no data or schema migration. Rollback is the prior complete install or
the single task commit; installer rollback already restores binaries and runtime
together.
