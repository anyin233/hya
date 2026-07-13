# Progress

`implement.md` is the persistent task plan; `research/` holds findings. This
file records execution evidence only.

## 2026-07-13

- User approved continuing from the reviewed artifacts.
- `task.py validate` passed with four real implementation and check context
  entries each.
- `task.py start 07-13-hya-ts-opencode-tui` changed the task to `in_progress`.
- Toolchain baseline: Bun `1.3.14`; Cargo `1.91.1`.
- Confirmed Slice 1 seam: retain upstream `run(TuiInput)` and direct SDK/SSE
  operation; exclude OpenCode backend, server, worker, provider runtime,
  updater, Console, and dynamic plugin loading.

### Slice 1 execution

- Rust baseline: `cargo test -p hya-sdk server::tests` passed (3 passed);
  `cargo test -p hya --test frontend_cli` passed (2 passed);
  `cargo test -p hya-server --test compat_event_api --test compat_permission_question_api`
  passed (11 + 5 passed); `cargo test -p hya-tui --test app_loop` passed
  (1 passed).
- RED: after adding only `packages/hya-tui-ts/test/boundary.test.ts`,
  `bun test test/boundary.test.ts` failed as intended with `ENOENT` for the
  missing package `LICENSE` (0 passed, 1 failed).
- Added the private Bun package shell, exact upstream MIT license, pinned
  provenance, Bun lockfile, pinned upstream dependencies, hya platform shim,
  local audio assets, and the retained upstream TUI source under
  `packages/hya-tui-ts/src/upstream`.
- Removed the Console organization and external plugin-manager modules and
  update action; replaced `@opencode-ai/core` and `@opencode-ai/ui` imports with
  hya/platform or local-asset equivalents. No backend, server, worker, provider
  runtime, updater, Console, or external plugin-loader import was required.
- Discovery: the published pinned SDK declaration exports the generated event
  union as `Event`, while this pinned TUI source used the checkout-only
  `V2Event` wrapper. `context/data.tsx` now derives the wrapper type locally
  from the pinned public `Event` properties without adding runtime code.
- Discovery: bundling dependencies made Bun resolve all optional OpenTUI native
  platforms. The prepared-runtime build keeps installed locked packages
  external via `--packages external`; no unused platform dependency was added.
- Lock generation: `bun install` passed (167 packages) and wrote `bun.lock`;
  validation `bun install --frozen-lockfile` passed with no changes.
- GREEN gate: `bun test test/boundary.test.ts` passed (1 test, 1755
  assertions); `bun run typecheck` passed; `bun run build` passed (162 modules,
  Bun target plus five audio assets).
- Review: `git diff --check` passed. Task-owned changes are confined to the new
  `packages/hya-tui-ts` package and this progress entry; upstream remained
  untouched.

### Slice 2 execution

- RED: after adding `test/sdk-spine.test.ts`, `bun test
  test/sdk-spine.test.ts` failed as intended with `Cannot find module
  '../src/main'` (0 passed, 1 failed, 1 error), proving the hya entry adapter
  and provider harness were absent.
- Added `src/main.tsx`: it parses the future launcher URL/project/session/model
  arguments, canonicalizes and changes to the selected project, resolves the
  default TUI config, creates the static built-in host, and runs the imported
  TUI with `HyaPlatform` supplied.
- Added `src/hya/static-host.ts` and `src/hya/sdk-spine.tsx`. The host starts only
  retained built-ins and disposes their handlers in reverse order. The harness
  mounts the real SDK, project, sync, and data providers without OpenTUI.
- The public-boundary test uses a recording local HTTP server and direct
  `/global/event` SSE. It verifies the canonical project and base URL reach the
  pinned SDK, bootstrap requests path/config/provider/catalog/session data, and
  a typed `session.updated` event reaches the existing sync store.
- Discovery: SDK v2 rewrites `x-opencode-directory` into the `directory` query
  parameter for GET/SSE requests; the test records the effective query value.
  No worker transport, OpenCode server/backend/provider runtime, updater,
  Console loader, dynamic plugin loader, or upstream Solid/OpenTUI patch was
  required.
- Changed Slice 2 files: `package.json`, `src/main.tsx`,
  `src/hya/static-host.ts`, `src/hya/sdk-spine.tsx`,
  `test/sdk-spine.test.ts`, and generated `dist/main.js`; removed stale
  `dist/index.js` and `dist/meta.json`. No dependency metadata changed, so no
  lockfile/install update was needed.
- GREEN: `bun test test/sdk-spine.test.ts` passed (1 test, 14 assertions);
  `bun test test/boundary.test.ts` passed (1 test, 1788 assertions); `bun run
  typecheck` passed; `bun run build` passed (180 modules, Bun target plus five
  audio assets, `main.js` entry).

### Slice 3 execution

- RED: after adding `test/branding-pruning.test.ts`, `bun test
  test/branding-pruning.test.ts` failed as intended because `src/hya/audit.ts`
  was missing and reachable source still registered unsupported controls.
- Added centralized hya product identities and one public audit seam for the
  logo/presentation name, terminal title, status command, config/state paths,
  default theme and sound pack, clipboard temp name, and static built-in IDs.
- Replaced the default theme identity and logo with hya, removed upstream theme
  links, and changed reachable status, notification, permission, help/error,
  footer, epilogue, and tip text to hya-owned presentation.
- Removed provider connection/offers, sharing, Console/org, remote workspace,
  stale docs links, retry upsells, plugin-manager keybinds/placeholders, and
  their now-dead dialogs/imports. Static built-ins remain sequential and their
  lifecycle handlers still dispose in reverse order.
- Strengthened `test/sdk-spine.test.ts`: the recording server rejects Console,
  provider-auth, upgrade/share, and remote-workspace endpoints, and the test
  asserts none were requested while retained bootstrap/event behavior passes.
- Changed Slice 3 areas: `src/hya/{audit,product,static-host}.ts`, branding and
  static registration wiring under `src/upstream`, theme assets, config/keybind
  identities, SDK bootstrap pruning, and `test/{branding-pruning,sdk-spine}.test.ts`.
  Deleted the unsupported provider, retry-action, workspace, move-session, and
  broken-workspace dialog/prompt modules.
- GREEN: `bun test test/branding-pruning.test.ts` passed (2 tests, 390
  assertions); `bun test test/sdk-spine.test.ts` passed (1 test, 15 assertions);
  `bun test test/boundary.test.ts` passed (1 test, 1605 assertions).
- Final Slice 3 gate: `bun test` passed (4 tests, 2010 assertions); `bun run
  typecheck` passed; `bun run build` passed (166 modules, Bun target plus five
  audio assets); `git diff --check` passed.

### Slice 4 execution

- Traced `PermissionRequests`, `QuestionRequests`, all three Compat SSE routes
  (`/event`, `/api/event`, `/global/event`), their existing envelope mappings,
  question reply/reject routes, the Rust SDK reducer, and the TypeScript sync
  reducer before editing.
- Added one public `/global/event` integration regression in
  `crates/hya-server/tests/compat_permission_question_api.rs`. It consumes
  `server.connected`, asks through the real session-scoped `InteractionPlane`,
  validates the SDK-compatible question ID/session/options, immediately replies,
  covers rejection, and proves wrong-session/duplicate routes emit no completion.
- RED: `cargo test -p hya-server --test compat_permission_question_api
  compat_global_event_streams_question_lifecycle_once -- --exact` failed as
  expected (0 passed, 1 failed) after one second waiting for the absent
  `question.asked` broadcast.
- Added the permission-style broadcast sender to `QuestionRequests`. Pending
  insertion now precedes `question.asked`; successful reply-channel completion
  precedes `question.replied`/`question.rejected`; unknown, wrong-session,
  duplicate, and failed-channel completions publish nothing. Existing atomic
  pending removal remains the exactly-once guard.
- Merged the shared question receiver into `/event`, `/api/event`, and
  `/global/event` through each route's existing payload wrapper. No persistence,
  engine event, generated SDK type, or TypeScript source changed.
- GREEN: the same focused command passed (1 passed); `cargo test -p hya-server
  --test compat_permission_question_api` passed (6 passed); `cargo test -p
  hya-server --test compat_event_api` passed (11 passed); `cargo test -p
  hya-server` passed all crate unit, integration, and doc tests.
- Related checks: `cargo test -p hya-sdk
  question_asked_then_rejected_lifecycle` passed (1 passed); `cargo test -p
  hya-tool interaction::tests` passed (2 passed); the existing TypeScript sync
  reducer already handles `question.asked`, `question.replied`, and
  `question.rejected`, so no focused TypeScript change/test was needed.
- Verification: `cargo fmt --all --check` passed; `cargo clippy -p hya-server
  --all-targets -- -D warnings` passed; `cargo build -p hya-backend --bin
  hya-backend` passed; `git diff --check` passed.
- One intermediate parallel check exposed `E0382` in the new test after adding
  the duplicate-reject assertion; cloning the test router for the preceding
  request fixed it, and both the complete test and clippy gates were rerun
  successfully.
- Changed Slice 4 files: `crates/hya-server/src/pending/question.rs`,
  `crates/hya-server/src/compat/event.rs`,
  `crates/hya-server/tests/compat_permission_question_api.rs`, and this progress
  entry.

### Slice 5 execution

- Re-read the approved launcher design, current `hya_sdk::ServerHandle`, existing
  `hya` HTTP backend resolution, workspace Cargo conventions, TypeScript entry
  arguments, and `hya` frontend CLI tests before editing. The workspace's
  `crates/*` glob already includes the new crate, so no root manifest edit was
  required.
- RED (parser/command seam): after adding only the new crate manifest, empty
  library, and `tests/launcher.rs`, `cargo test -p hya-ts --test launcher`
  failed with unresolved imports for `Cli`, `build_bun_command`,
  `build_bun_command_from`, and `resolve_runtime_dir`.
- GREEN (parser/command seam): added the public clap contract, canonical project
  resolution, HTTP(S) URL validation, pre-spawn `--fork` validation, Bun command
  construction, and runtime resolution order (`HYA_TUI_TS_DIR`, installed
  sibling, workspace package). The focused command passed 4 tests.
- RED (process seam): with an empty `main`, `cargo test -p hya-ts --test process`
  failed all 3 tests: attached mode returned 0 instead of Bun's 23, owned mode
  produced no backend evidence, and handled termination never reached Bun.
- GREEN (process seam): the launcher now attaches without server ownership or
  termination, or calls `ServerHandle::spawn_hya_backend` and retains exactly
  that handle through Bun exit. Bun runs from the runtime package with
  `src/main.tsx`, canonical `--project`, `--url`, and all public TUI arguments;
  its numeric exit status is propagated, with generic status 1 for signals or
  no-code exits. SIGINT/SIGTERM terminate Bun's process group before dropping
  the owned handle.
- Fake executable process tests passed: attached mode forwards URL/project/TUI
  arguments, returns Bun status 23, and leaves an unrelated server alive; owned
  normal exit removes the backend process group and announced listener; handled
  SIGTERM removes Bun, the owned backend group, and its announced listener.
- `hya-sdk` needed no edit: `ServerHandle` already provided all required
  lifecycle ownership and cleanup operations. No second backend supervisor was
  introduced.
- Slice 5 files: new `crates/hya-ts/{Cargo.toml,src/lib.rs,src/main.rs,
  tests/launcher.rs,tests/process.rs}`, the generated `Cargo.lock` package entry,
  and this progress entry. Current `hya` and Rust TUI sources were unchanged.
- Final GREEN: `cargo test -p hya-ts` passed (4 launcher + 3 process tests);
  `cargo fmt --all --check` passed; `cargo clippy -p hya-ts --all-targets -- -D
  warnings` passed; `cargo build --locked -p hya -p hya-backend -p hya-ts
  --bins` passed; `cargo test -p hya --test frontend_cli` passed (2 tests);
  `./target/debug/hya --help` printed the existing Rust frontend help;
  `./target/debug/hya-ts --help` printed the new launcher contract; `git diff
  --check` passed.
- Scope limitation: process and signal handling is Unix/Linux-only as approved.

### Slice 6 execution

- Added `packages/hya-tui-ts/test/real-backend.test.ts`. It starts a built
  `target/debug/hya-backend` with an isolated HOME/XDG state, SQLite database,
  project directory, ephemeral loopback port, and the pinned
  `createOpencodeClient` from `@opencode-ai/sdk/v2`; no fake transport or
  generated SDK edit is involved.
- The real SDK tracer covers the retained path/project/config/provider/model,
  agent, command, session list/status, file list/read/status/find, MCP, LSP, and
  formatter bootstrap calls; `/global/event` decoding; session
  create/get/list/update/delete; async prompt and streamed dev-provider text;
  shell tool activity; busy status and abort; messages/todos/diff; and
  permission/question list decoding. The isolated server uses existing
  `--yolo` because an unattended test has no TUI permission responder.
- First Bun RED: SDK `session.create({ title: "SDK workflow" })` returned the
  generated fallback title. Focused Rust RED
  `compat_legacy_session_create_preserves_requested_title` reproduced the exact
  `POST /session` body and received `Untitled Session_...`; the shared legacy
  create handler now applies its optional title through `SessionEngine::set_title`.
- Second Bun RED: SDK `session.promptAsync` received HTTP 422 because its public
  request uses `parts`, while the async handler only accepted native `text`.
  Focused Rust RED `compat_prompt_async_accepts_sdk_parts_body` reproduced the
  exact body and received 422; the async route now reuses the existing legacy
  prompt payload/text extraction used by synchronous SDK prompts.
- Added `packages/hya-tui-ts/test/pty-smoke.test.ts`. On Linux it uses the
  platform `/usr/bin/script`, a fixed 30x100 PTY, a real temporary backend, and
  the package Bun entry to render hya, auto-submit `PTY session smoke`, show the
  streamed offline reply and session screen, exit via the configured Ctrl-C
  binding, and compare pre/post `stty -g` terminal state. No PTY dependency was
  added.
- PTY limitation: invoking the same smoke through `hya-ts` does not render in
  `script` because the launcher starts Bun in a new process group without making
  it the PTY foreground group. Slice 6's allowed edit surface excludes the
  Slice 5 launcher, so the committed smoke exercises the exact Bun command that
  `hya-ts` constructs against the same real backend and records this follow-up
  rather than silently expanding scope.
- Permission/question deterministic triggers were not added to the dev provider.
  Their real route and `/global/event` reply/reject lifecycle remains covered by
  Slice 4's `compat_permission_question_api`; this tracer verifies pinned SDK
  list decoding against the real process.
- Focused GREEN: `cargo test -p hya-server --test compat_session_title_api
  --test compat_prompt_async_api --test compat_permission_question_api --test
  compat_event_api` passed (5 + 3 + 6 + 11 tests); `bun test
  test/real-backend.test.ts` passed (1 test, 12 assertions); `bun test
  test/pty-smoke.test.ts` passed (1 test, 5 assertions).
- Package gate: `bun install --frozen-lockfile` passed with no changes; `bun run
  typecheck` passed; `bun test` passed 6 tests across 5 files with 2027
  assertions; `bun run build` passed (166 modules and five audio assets).
- Repository gate: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo build
  --locked -p hya -p hya-backend -p hya-ts --bins` all passed. `git diff
  --check` passed.
- Slice 6 files: `packages/hya-tui-ts/test/{real-backend,pty-smoke}.test.ts`,
  `crates/hya-server/src/compat/{session_create_legacy,session_prompt_async,
  session_prompt_legacy}.rs`, `crates/hya-server/tests/{compat_session_title_api,
  compat_prompt_async_api}.rs`, and this progress entry.

### Slice 6 launcher PTY correction

- Root cause: `hya-ts` retained Bun in a dedicated process group for subtree
  cleanup but did not transfer the controlling terminal foreground group to it.
  OpenTUI's terminal ioctl was therefore stopped by `SIGTTOU`.
- RED: `cargo test -p hya-ts --test process
  attached_mode_gives_bun_the_terminal_and_restores_it -- --exact` failed
  (0 passed, 1 failed) with `bun pgid=598138 foreground=598099` and fake-Bun
  exit 91. `bun test test/pty-smoke.test.ts` through the launcher failed (0
  passed, 1 failed) with status 130 before the visible rendering assertions.
- Added a Linux `/usr/bin/script` launcher regression whose fake Bun requires
  its PGID to equal the TTY foreground PGID, changes terminal mode, and exits;
  the outer shell requires exact pre/post `stty -g` and foreground-PGID equality.
- `hya-ts` now captures the controlling TTY's exact foreground PGID and termios,
  rejects background invocation, hands the foreground to Bun while blocking
  `SIGTTOU`, sends `SIGCONT`, and explicitly reclaims the previous foreground
  PGID before restoring termios with `TCSANOW`. Non-TTY stdin remains a no-op.
  Explicit restoration precedes owned-backend handle drop, with a best-effort
  `Drop` retry. Handoff failure terminates/reaps Bun; launcher termination sends
  `SIGTERM`, then `SIGCONT`, waits one second, then uses `SIGKILL` and reaps.
- The real package PTY smoke now invokes built `hya-ts` in attached mode with
  `HYA_TUI_TS_DIR`; it retains the real backend, hya/session/prompt/reply
  assertions, bounded Ctrl-C exit, terminal-mode comparison, and foreground
  process-group comparison.
- GREEN: focused launcher PTY test passed (1 passed); full process test passed
  (4 passed); `cargo test -p hya-ts` passed (4 launcher + 4 process tests);
  `cargo fmt --all --check` passed; `cargo clippy -p hya-ts --all-targets -- -D
  warnings` passed; `cargo build --locked -p hya-ts -p hya-backend --bins`
  passed; launcher-level `bun test test/pty-smoke.test.ts` passed (1 test, 5
  assertions); package `bun test` passed (6 tests, 2027 assertions); `bun run
  typecheck` passed; `bun run build` passed (166 modules and five assets); `git
  diff --check` passed.
- Changed files: `crates/hya-ts/src/main.rs`, `crates/hya-ts/tests/process.rs`,
  `packages/hya-tui-ts/test/pty-smoke.test.ts`, and this progress entry.
- Known ceiling: full Ctrl-Z suspend/resume relay remains separate and was not
  implemented or claimed.
- Execution notes: the first implementation `apply_patch` found stale context
  and changed nothing; rereading `main.rs` resolved it. The first post-edit
  `cargo fmt --all --check` reported only rustfmt layout, which was applied
  before all recorded GREEN checks.

### Slice 7 execution

- Installer RED: after extending `tests/install_script.sh` first, `bash
  tests/install_script.sh` failed as intended with `FAIL: expected output to
  contain: hya-ts` against the two-binary installer.
- Installer GREEN: `bash tests/install_script.sh` passed. Its isolated fake
  Cargo/Bun fixture verifies Bun preflight precedes the three-package Cargo
  build, frozen production preparation, all three executable smokes, the
  existing `hya` PATH check, and rollback after a forced post-placement
  `hya-ts --help` failure. The rollback restores all four prior install units
  and leaves no installer `.tmp` or `.bak` paths.
- Installed layout is `<prefix>/bin/{hya,hya-backend,hya-ts}` plus
  `<prefix>/lib/hya/hya-tui-ts/{package.json,bun.lock,LICENSE,UPSTREAM.md,src/,
  node_modules/}`. Direct `--bin-dir` derives sibling `../lib/hya`. The staged
  runtime copies source/legal/lock files, runs `bun install
  --frozen-lockfile --production` only in that copy, and excludes package
  `test`, `dist`, `tsconfig.json`, and `bunfig.toml`; source `node_modules` is
  not mutated or deleted.
- CI and release install Bun `1.3.14` through Bun's official installer with the
  explicit `bun-v1.3.14` release argument and verify the reported version. CI
  runs frozen install, typecheck, build, builds the two binaries required by
  the real-backend/PTY tests, then runs all Bun tests before the normal Rust
  format/clippy/build/test gates.
- Release keeps archive name `hya-<version>-x86_64-unknown-linux-gnu.tar.gz` and
  packages `bin/{hya,hya-backend,hya-ts}` plus prepared
  `lib/hya/hya-tui-ts`. Its smoke checks all binaries, exact `LICENSE` and
  `UPSTREAM.md`, entry/lock/package files, production `node_modules`, and tar
  entries while retaining SHA256, attestation, artifact, and release steps.
- Versions are `0.33.0` in the workspace, every workspace `Cargo.lock` package,
  and `packages/hya-tui-ts/package.json`; `bun.lock` did not change. Exact prior
  root changelog content is archived as
  `docs/changes/CHANGELOG_0.32.4.md`; root `CHANGELOG.md` contains only 0.33.0.
- GREEN package gate: `bun install --frozen-lockfile`, `bun run typecheck`,
  `bun test` (6 passed, 2027 assertions), and `bun run build` (166 modules and
  five assets) passed.
- GREEN Rust/repository gates: `cargo metadata --locked --no-deps
  --format-version 1`, `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo build
  --locked -p hya -p hya-backend -p hya-ts --bins` passed; the focused build
  compiled all three executables at version 0.33.0.
- Shell/workflow/release checks: `bash -n install.sh tests/install_script.sh`
  passed; PyYAML 5.4.1 parsed both workflows and `bash -n` accepted all 11 CI
  and 7 release embedded shell blocks; representative version/changelog checks
  passed, including byte-for-byte comparison of the archived changelog; a
  local representative archive passed all three binary, runtime, legal, and
  tar-entry assertions; `git diff --check` passed.
- Verification limitation: GitHub Actions itself, the release-target optimized
  archive build, attestation/upload, and publishing were not run locally. No
  tag, release, commit, or push was created. One first local archive-check
  attempt accidentally assigned zsh's special `path` variable and invalidated
  that subprocess; its known temporary directory was removed and the corrected
  check above passed.
- Slice 7 files: `tests/install_script.sh`, `install.sh`,
  `.github/workflows/{ci,release}.yml`, `Cargo.toml`, `Cargo.lock`,
  `packages/hya-tui-ts/package.json`, `CHANGELOG.md`, new
  `docs/changes/CHANGELOG_0.32.4.md`, and this progress entry. Existing `hya`,
  unrelated dirty paths, `packages/hya-tui-ts/bun.lock`, and the untracked
  `docs/changes/CHANGELOG_0.32.1.md` were left untouched.

### Independent-check correction: real SDK permission lifecycle

- RED: a new non-yolo real-backend test drove `session.shell` through the pinned SDK and received `permission.asked`, but its required `/global/event` `directory` was `undefined`. After the shared envelope fix, the same test reached the next RED: `permission.asked.properties.always` was `undefined`.
- The shared `/global/event` envelope now carries the backend project directory for connected, engine, permission, question, and heartbeat events. The heartbeat stream accepts a closure so the same directory owner is reused without global state.
- Root `/permission` and `permission.asked` now share one typed legacy SDK view (`permission/patterns/metadata/always/tool`); existing `/api/*` V2 endpoints retain their distinct `action/resources/save` view.
- GREEN: `bun test test/real-backend.test.ts --test-name-pattern "pinned SDK resolves real shell permissions exactly once"` passed (1 test, 11 assertions). It covers once/reject, present/absent file side effects, duplicate 404s, exactly one completion event, and final empty pending state.
- Focused Rust checks passed: the global connected-event contract, the root permission route contract, and a rebuilt `target/debug/hya-backend` used by the Bun test.

### Independent-check correction: question and remaining boundary failures

- Question lifecycle GREEN: the real pinned SDK test covers reply/reject, answer propagation, duplicate `404`s, exactly-once completion events, empty pending state, streamed idle status, and provider tool-result continuation. Full `real-backend.test.ts` passed 3 tests with 34 assertions.
- The combined Rust gate reached all passing workspace/doc-test summaries and the final locked executable build (`Finished dev profile`); no failure marker occurred.
- An isolated read-only Trellis check rerun confirmed the global/question and real-SDK lifecycle corrections, then returned `FAIL` on four remaining classes: unused SDK server entrypoints in prepared runtimes, one JSX `opencode mcp auth` branding leak, one dead Console provider helper copied into the runtime, and mutable GitHub Actions refs.
- Source audit RED: `branding-pruning.test.ts` failed on `upstream/util/provider-origin.ts`; the strengthened pattern also covers the JSX command and stale console-error promise. Deleted the dead helper and replaced both reachable messages with hya-valid text. Focused audit GREEN: 2 tests, 388 assertions.
- Installer/runtime RED: `tests/install_script.sh` first failed because no SDK-pruning step existed, then—after adding that step—failed only on `actions/checkout@v4`. Added one preparation script that removes the pinned SDK's `./server`/`./v2/server` exports and server/process files after production install; installer and release use the same script and verify client files remain.
- Workflow refs now use immutable upstream commits for checkout v4.2.2, Rust stable, rust-cache v2.7.8, upload-artifact v4.6.2, and download-artifact v4.3.0. Focused installer/runtime/workflow check passed.
- Real prepared-runtime verification passed under ignored `target/hya-ts-final-install`: the installer used Bun 1.3.14 and the exact production lockfile, retained both SDK client entrypoints, removed both server exports plus server/process files, and `@opencode-ai/sdk/v2` still imported successfully. The first equivalent attempt under `/tmp/opencode` stopped safely before building because that directory belongs to another user and is not writable.
- Final Bun gate passed: frozen install unchanged, typecheck passed, 8 tests passed with 2,044 assertions, and the 166-module build plus five audio assets passed.
- Final repository gate passed: `cargo fmt --all --check`, workspace/all-target clippy with warnings denied, all workspace tests and doc-tests, and locked debug builds of `hya`, `hya-backend`, and `hya-ts`. Workflow validation parsed both YAML files, syntax-checked all 18 embedded shell blocks, asserted every action uses a 40-character commit, reran the installer fixture, and passed `git diff --check`.
- Fresh isolated read-only final review returned `PASS` with zero blockers across all prior failures, launcher ownership, install/release, version/changelog, and legal/source boundaries. The reviewer independently passed formatting, clippy, focused Compat/launcher/build checks, installer smoke, TypeScript typecheck/build, and diff whitespace; its socket sandbox could not run the full Bun integration suite, which the main session had already run successfully above.
- Durable executable contracts were added to backend/frontend quality specs for root Compat interaction envelopes/exactly-once completion and prepared-runtime SDK client retention/server-entrypoint pruning.
- Committed the isolated task-owned change as `44dd829e` (`feat: add TypeScript TUI frontend`) and pushed it to `origin/main`; unrelated workspace changes remained unstaged.
