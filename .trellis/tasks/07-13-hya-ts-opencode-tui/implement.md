# Implementation Plan

## Gate

Do not implement until the user approves these artifacts and the task is
activated with:

```sh
python3 ./.trellis/scripts/task.py start 07-13-hya-ts-opencode-tui
```

Preserve all unrelated changes in the currently dirty workspace. Stage only
task-owned files after every required check passes.

## Slice 1: Freeze The Upstream And Legal Boundary

1. Run focused Rust baselines for the current launcher, SDK server handle,
   Compat event/question APIs, and existing frontend.
2. Create the minimal `packages/hya-tui-ts` test/tooling shell.
3. Add one failing Bun boundary test that requires the exact upstream MIT text,
   pinned provenance, allowed package imports, and absence of backend/server/
   provider/worker/updater/Console modules.
4. Observe RED for the missing package content.
5. Add `LICENSE`, `UPSTREAM.md`, exact dependency pins, and the selectively
   copied `src/upstream` source/assets needed by the retained TUI.
6. Add hya-owned entry/runtime files only when required to make the boundary
   test, frozen install, typecheck, and build pass.

Gate:

```sh
cd packages/hya-tui-ts
bun install --frozen-lockfile
bun test test/boundary.test.ts
bun run typecheck
bun run build
```

Stop instead of widening scope if the build requires an OpenCode backend,
provider runtime, worker, updater, Console, or external plugin loader.

## Slice 2: Direct SDK Rendering Spine

1. Add a failing test around the upstream SDK provider using a recording fake
   server: bootstrap path/config/catalog/session requests and one
   `/global/event` payload must reach the existing sync/data path.
2. Observe the test fail without the hya entry adapter.
3. Add the smallest `src/main.tsx` adapter: parse launcher arguments, change to
   the selected project, build default TUI config, create the static built-in
   host, and call the imported TUI with the supplied hya URL.
4. Replace only reachable `@opencode-ai/core` utilities with local Node/Bun
   implementations and hya paths. Copy required frontend audio assets locally
   or remove a sound path if no retained test requires it.
5. Preserve the upstream Solid/OpenTUI preload and patch only if an executable
   failing test reproduces the upstream Solid defect.
6. Port and run the upstream lifecycle, renderer, SDK context, theme, prompt,
   permission, and question tests relevant to retained behavior.

Gate: bootstrap requests and one streamed event update the rendered state with
no OpenCode backend process or worker transport.

## Slice 3: Prune Unsupported Features And De-brand

1. Add failing branding snapshots/audits for home, session, permission,
   question, terminal title, status/help, config/state paths, and packaged text.
2. Add failing tests proving removed OpenCode-only commands/screens are absent.
3. Replace the logo and reachable product text with `hya`.
4. Rename local default theme/sound/config/state identities to hya and remove
   stale product links rather than inventing undocumented URLs.
5. Remove update, sharing, Console/org, remote workspace, OpenCode provider
   offers, dynamic TUI plugin loading, and Plugin Manager imports/commands.
6. Implement the minimum static built-in host needed by retained layout slots;
   initialize built-ins sequentially and dispose registered lifecycle handlers
   in reverse order.
7. Re-run the import boundary test to prove pruning happened at the dependency
   graph, not only in visibility flags.

Gate: reachable snapshots contain hya branding, allowed OpenCode strings appear
only in SDK/protocol/legal/provenance contexts, and no unsupported control is
registered.

## Slice 4: Stream Question Lifecycle Events

1. Add a failing `hya-server` integration test that subscribes to
   `/global/event`, consumes `server.connected`, issues a scoped question, and
   expects `question.asked`, then reply and reject completion events.
2. Observe RED because `QuestionRequests` has no broadcast channel.
3. Add the existing permission-style broadcast sender to `QuestionRequests`.
4. Publish `question.asked` only after pending insertion; publish
   `question.replied` or `question.rejected` only after successful completion.
5. Merge the question stream into `/event`, `/api/event`, and `/global/event`
   using their existing envelope shapes.
6. Re-run existing question routes, permission events, SDK reducer, and new TUI
   question-panel tests.

Gate: ask/reply/reject each appear once with the SDK-required IDs and answer
labels; immediate replies cannot race insertion.

## Slice 5: Add The hya-ts Launcher

1. Create `crates/hya-ts` with failing parser/command-construction tests for the
   selected project, session/fork validation, attached URL, Bun, backend binary,
   and forwarded TUI arguments.
2. Add failing process tests with fake Bun/backend executables for owned versus
   attached lifecycle, exit-code propagation, and cleanup on normal exit and
   handled termination.
3. Reuse `hya_sdk::ServerHandle` for owned backend startup. Extend that existing
   seam only if a test proves the launcher needs a missing operation.
4. Resolve runtime assets from an explicit development override, installed
   sibling `lib/hya/hya-tui-ts`, then the workspace package.
5. Spawn Bun from the runtime package, forward the hya URL/project/TUI args,
   handle termination, and propagate its exit status.
6. Build `hya`, `hya-backend`, and `hya-ts`; verify current `hya --help` and
   startup behavior are unchanged.

Gate: attached servers survive `hya-ts` exit; owned backend process groups and
ports do not.

## Slice 6: Real Backend Compatibility Workflow

1. Add a Bun integration test that starts a temporary hya backend and exercises
   the exact retained SDK bootstrap methods.
2. Exercise session create/list/get/messages, prompt submission, streamed text
   and tool activity, abort, permission reply/reject, and question reply/reject.
3. Exercise retained model/agent/command/file/MCP/LSP/formatter screens through
   their SDK calls.
4. For each failure, first add a focused Rust Compat test reproducing the exact
   request. Fix the shared handler only after RED; otherwise remove an
   OpenCode-specific frontend assumption.
5. Add a Linux pseudo-terminal smoke that renders the hya home screen, opens a
   session, and exits with restored terminal state. Reuse platform `script` or
   existing test infrastructure; add no PTY dependency.

Gate: AC1 through AC5 pass without importing OpenCode backend code.

## Slice 7: Install, CI, Release Layout, And Version

1. Add failing installer tests for the expected three binaries, prepared TUI
   runtime, Bun preflight, legal files, and all-or-nothing rollback.
2. Extend `install.sh` minimally to build/install `hya-ts` and the frozen
   production TUI runtime under the sibling `lib/hya` directory.
3. Add pinned Bun setup and package-local checks to CI before Rust checks.
4. Extend release packaging and smoke tests to include `hya-backend`, `hya-ts`,
   the prepared runtime, `LICENSE`, and `UPSTREAM.md`, while retaining `hya`.
5. Bump `[workspace.package].version` and the TypeScript package to `0.33.0`.
6. Move root `CHANGELOG.md` to `docs/changes/CHANGELOG_0.32.4.md` and create a
   root changelog containing only `0.33.0` notes.
7. Do not create a tag or publish a release.

## Final Verification

Run package checks:

```sh
cd packages/hya-tui-ts
bun install --frozen-lockfile
bun run typecheck
bun test
bun run build
```

Run repository checks and local builds:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --locked -p hya -p hya-backend -p hya-ts --bins
bash tests/install_script.sh
```

Also run the focused PTY, SDK/real-backend, source-boundary, branding,
attribution, archive-content, and process-cleanup checks from the slices above.
Review `git diff --check`, the complete task diff, and `git status --short`.

After all gates pass, run `trellis-check`, update durable specs only for newly
learned contracts, create one task-scoped semantic feature commit, and push it
without staging unrelated dirty paths.

## Rollback Points

- Slice 1: delete the new package if the retained TUI cannot be isolated from
  OpenCode backend modules.
- Slice 3: remove a dependent screen instead of restoring unsupported services.
- Slice 4: revert the isolated question broadcast bridge; it has no schema or
  persistence change.
- Slice 5: remove `hya-ts`; current `hya` remains the working frontend.
- Slice 7: installer rollback restores all previous binaries and assets as one
  unit.
