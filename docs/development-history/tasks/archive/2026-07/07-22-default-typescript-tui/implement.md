# Implementation Plan

## Gate

Do not edit production code until the user reviews these artifacts and the task
is activated with:

```sh
python3 ./.trellis/scripts/task.py start 07-22-default-typescript-tui
```

Before the first edit, load `trellis-before-dev`, re-read the live workspace
version/changelog, inspect `git status`, and record any concurrent changes.
Preserve unrelated work and stage only this task's files.

## Slice 1: Complete The Shared Launcher CLI

1. Add one `hya-ts` process test for `--import compat` using isolated config
   paths and deliberately unusable backend/Bun paths. Require the existing
   import summary and prove neither process starts.
2. Run that test and observe RED because `hya-ts` does not accept `--import`.
3. Add the existing `hya-app` dependency, parse the option in the shared Clap
   CLI, and move the smallest existing import wrapper into `hya-ts`. Return
   before project/runtime/backend/Bun resolution.
4. Re-run the focused import test to GREEN.
5. Add one process test requiring a missing `--bun` executable to fail nonzero,
   name the attempted executable, and avoid fallback. Observe RED, add context at
   the existing spawn boundary, and rerun GREEN.

Gate:

```sh
cargo test -p hya-ts --test process
```

Do not alter the existing process supervisor, `--backend-bin`, or backend
ownership behavior.

## Slice 2: Replace The Default Frontend

1. Build `hya-ts`, then replace the obsolete non-TTY frontend test with one
   failing `hya` process test using the existing fake Bun/runtime pattern. It
   must prove forwarded arguments, Bun exit status, and that Bun's parent PID is
   the original `hya` PID.
2. Observe RED against the Rust frontend.
3. Replace `crates/hya/src/main.rs` with the adjacent-only standard-library Unix
   exec shim, forwarding all arguments and setting `argv[0]` to `hya`.
4. Re-run the delegation test to GREEN.
5. Add focused RED tests for `hya` help/version branding and for an explicitly
   relocated shim with no adjacent `hya-ts`. Require a path-bearing nonzero
   error for the missing sibling.
6. Make the existing Clap parser choose `hya` or `hya-ts` presentation from the
   invocation name and add version metadata. Keep direct `hya-ts` presentation
   unchanged. Re-run both entrypoint tests to GREEN.
7. Keep the existing `hya --import compat` integration fixture and verify it now
   passes through the shared launcher.
8. Delete `crates/hya/src/{args,backend,events,transport}.rs`. Remove obsolete
   normal dependencies from `crates/hya/Cargo.toml`; retain only the
   dev-dependencies required by `native_round_trip.rs` and version/import tests.
9. Run the native no-socket test and its repository script unchanged.

Gate:

```sh
cargo build -p hya-ts --bin hya-ts
cargo test -p hya --test frontend_cli
cargo test -p hya --test native_round_trip
bash scripts/verify-no-http.sh
```

## Slice 3: Route Bare Backend Resume Correctly

1. Change the existing `hya_launch_args_forwards_resume_session` expectation to
   `--session <id>` and observe RED.
2. Change only the generated child flag in `crates/hya-backend/src/serve.rs`.
3. Re-run the focused test to GREEN and retain `HYA_FRONTEND_BIN` behavior.

Gate:

```sh
cargo test -p hya-backend hya_launch_args_forwards_resume_session
```

## Slice 4: Prove Installed And Packaged Startup

1. Extend `tests/install_script.sh` first so it requires installer help to call
   `hya` the TypeScript frontend and requires a post-install no-op Bun smoke
   through canonical `hya`. Observe RED.
2. Update `install.sh` wording and add the noninteractive attached-mode smoke,
   preserving the existing three-binary/runtime transaction and rollback.
3. Add a static release-workflow assertion to the installer test, observe RED,
   then add the equivalent packaged `hya` no-op Bun smoke to
   `.github/workflows/release.yml`.
4. Add `hya` to the explicit frontend binary build in CI; do not change the
   package layout or add another build path.

Gate:

```sh
bash tests/install_script.sh
cargo build --locked -p hya -p hya-backend -p hya-ts --bins
./target/debug/hya --server http://127.0.0.1:1 --bun /bin/true
```

## Slice 5: Documentation And Release Metadata

1. Search active source, scripts, workflows, README, and docs for claims that
   Rust `hya` is current/default, old direct-resume syntax, and standalone
   `cargo install --path crates/hya`. Do not edit historical changelogs or
   archived task records.
2. Update only active user and architecture documentation, including `README.md`,
   `docs/getting-started.md`, `docs/cli.md`, `docs/configuration.md`, `install.sh`,
   and the current project-agent component map where stale.
3. Re-read the live version. If it remains `0.33.28`, bump workspace and
   TypeScript package metadata to `0.33.29`, update `Cargo.lock` and README
   metadata, move root `CHANGELOG.md` to
   `docs/changes/CHANGELOG_0.33.28.md`, and write a `0.33.29` root changelog.
   If the baseline advanced, derive the next patch and archive that live root
   changelog instead.
4. Run `version_metadata` and a scoped stale-reference search.

Gate:

```sh
cargo test -p hya --test version_metadata
```

## Final Verification

Run the TypeScript runtime checks:

```sh
cd packages/hya-tui-ts
bun run typecheck
bun test
bun run build
```

Run repository checks and local builds from the workspace root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --locked -p hya -p hya-backend -p hya-ts --bins
bash tests/install_script.sh
bash scripts/verify-no-http.sh
```

Smoke both colocated aliases and the canonical attached path from the same build
directory. Then run `git diff --check`, inspect the complete diff and
`git status --short`, and verify no Rust TUI executable or fallback remains.

Run `trellis-check`, update durable specs only for newly learned contracts, and
complete the project-required atomic semantic commit and push only after every
gate passes. Do not create a release tag or publish a release.

## Rollback Points

- Slice 1: revert only the import/parser additions; no config schema changes.
- Slice 2: restore the previous complete executable set from one commit or
  install, never a mixed `hya`/`hya-ts` pair.
- Slice 3: revert the isolated flag translation if the attached-session test
  disproves the contract.
- Slice 4: the existing installer transaction restores all binaries and runtime
  together on any smoke failure.
