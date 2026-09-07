# Design: hya-ts with the OpenCode TUI

## Chosen Architecture

```text
hya-ts (new Rust launcher)
  -> attach to --server, or own hya-backend serve on loopback
  -> run Bun with packages/hya-tui-ts
  -> @opencode-ai/sdk/v2 HTTP + /global/event SSE
  -> hya-server Compat routes
  -> hya-core and hya-store
```

The TypeScript side is a frontend only. Backend startup, readiness, ownership,
and cleanup remain in Rust by reusing `hya_sdk::ServerHandle`. The TUI talks
directly to hya's existing Compat API through the upstream SDK network seam; the
OpenCode worker/RPC transport is not imported.

## Planner Merge

Two independent plans agreed on the source boundary, direct SDK/HTTP/SSE seam,
static-only TUI components, API-gap-by-evidence rule, de-branding, MIT notice,
and coexistence with the Rust frontend.

They differed in two areas:

- Launcher ownership: one plan recommended a Rust supervisor reusing
  `ServerHandle`; the other considered a Bun-owned or compiled launcher. The
  Rust supervisor is selected because its readiness parsing and process-group
  cleanup already exist and are tested. Reimplementing them in Bun would create
  a second lifecycle path.
- Distribution: one plan favored a prepared source runtime while the other
  favored a compiled Bun executable. The user selected the prepared,
  Bun-dependent runtime. Self-contained compilation is deferred.

Both plans suggested preserving a plugin-shaped runtime for upstream layout.
The merged design keeps only a minimal static built-in host. It deletes external
plugin discovery, installation, activation management, and the Plugin Manager
screen rather than adding a no-op compatibility framework.

## Repository Layout

```text
crates/hya-ts/
  Cargo.toml
  src/main.rs
  tests/...

packages/hya-tui-ts/
  package.json
  bun.lock
  bunfig.toml
  tsconfig.json
  LICENSE
  UPSTREAM.md
  src/main.tsx
  src/hya/...
  src/upstream/...
  test/...
```

`packages/` is outside the Cargo workspace glob. `src/upstream/` is the
selectively imported and modified TUI boundary. `src/hya/` owns the direct
launcher adapter, local paths/runtime values, and static built-in host. This
directory split makes provenance and later upstream comparison explicit without
adding a sync generator before one is needed.

The package is private. Dependency versions are pinned from the upstream 1.17.9
lockfile. `@opencode-ai/sdk/v2` remains the compatibility client, and the TUI
plugin contract may remain an upstream package dependency. `@opencode-ai/core`
and `@opencode-ai/ui` are not runtime dependencies: the few required path,
flag, file, executable lookup, and audio-asset needs are replaced locally or by
standard platform APIs.

## TUI Runtime Boundary

The imported boundary starts from upstream `packages/tui/src/index.tsx` and
`app.tsx`, plus only files and assets reachable by retained screens and static
built-ins.

Retained behavior includes:

- home, session, transcript, prompt, dialogs, themes, clipboard, selection, and
  terminal lifecycle;
- session list/create/resume/rename/delete/fork where the existing API supports
  it;
- model, variant, and agent selection;
- slash commands and file completion;
- streamed text, reasoning, tool, status, todo, permission, and question UI;
- static home/footer/sidebar/diff/notification/which-key components whose
  backing hya endpoints pass contract tests.

Removed behavior includes:

- OpenCode worker/RPC and server startup;
- update checks and `global.upgrade` UI;
- Console/organization and OpenCode-specific provider offers;
- sharing/public-link actions;
- remote workspace creation, switching, movement, and synchronization;
- external TUI plugin discovery, installation, activation, and management;
- OpenCode docs, issue, provider, and product links that have no hya target.

Removal happens at imports and command registration, not by leaving visible
disabled controls. An import-boundary test blocks accidental reintroduction.

## hya-ts Launcher

`hya-ts` is a small Rust binary, not a second frontend implementation. Its
useful CLI maps to the imported TUI:

```text
hya-ts [project]
       [--server URL]
       [--backend-bin PATH]
       [--bun PATH]
       [--continue]
       [--session ID]
       [--fork]
       [--prompt TEXT]
       [--agent NAME]
       [--model PROVIDER/MODEL]
```

The launcher validates incompatible session/fork arguments before spawning
anything. It resolves Bun and the installed TUI directory, then:

- attach mode keeps only the supplied URL and never owns or terminates the
  remote process;
- default mode resolves `hya-backend`, calls
  `ServerHandle::spawn_hya_backend`, and retains the handle until Bun exits;
- project defaults to the canonical current directory;
- Bun starts from the installed package for dependency/preload resolution, and
  `src/main.tsx` changes to the project directory before loading the TUI;
- normal exit and handled termination signals stop Bun, restore terminal state,
  and drop only the owned backend handle;
- Bun's exit status is propagated.

No new database default is introduced. The owned backend keeps current hya
semantics; persistent or shared sessions can be supplied through an explicitly
managed hya server. Persistence policy is separate from this frontend
migration.

## Backend Contract

The generated SDK v2 request and response types are the client-side contract.
Tests exercise the exact methods used during bootstrap and core workflows
against a real temporary hya backend. A failed request is recorded as method,
path, request body, expected response/event, and observed response before any
server edit.

The first confirmed gap is question event delivery:

```text
InteractionPlane
  -> QuestionRequests inserts pending request
  -> question.asked broadcast
  -> /event, /api/event, /global/event
  -> SDK reducer and TUI question panel
  -> reply/reject route
  -> QuestionRequests resolves channel
  -> question.replied/question.rejected broadcast
```

`QuestionRequests` will follow the existing `PermissionRequests` broadcast
pattern. Insertion precedes `question.asked` publication so an immediate reply
cannot race the pending map. Completion events publish only after the reply
channel succeeds. Event shapes remain the SDK-compatible shared contract; the
TUI does not parse an alternative hya-only payload.

Any additional Compat change requires a focused failing test driven by a real
retained TUI SDK call. OpenCode-backend-specific assumptions are removed from
the frontend instead of recreated in hya.

## Branding And Attribution

De-branding applies to reachable product presentation:

- logo and startup identity;
- terminal titles and command labels;
- default theme and sound-pack names;
- help, tips, errors, docs/provider/update links;
- local config, state, cache, and temporary-file paths.

The visible product spelling is `hya`. Internal generated SDK names, dependency
package names, protocol fields, source comments, `LICENSE`, and `UPSTREAM.md`
are explicitly excluded from the branding ban.

`packages/hya-tui-ts/LICENSE` contains the upstream MIT text unchanged.
`UPSTREAM.md` records the repository, version, commit, imported source/assets,
excluded systems, and that hya modified and rebranded the TUI. Both files ship
with every installed/runtime package and release archive.

## Installation And Release Layout

The source install builds `hya`, `hya-backend`, and `hya-ts`, verifies Bun,
performs a frozen production install for `hya-tui-ts`, and atomically installs:

```text
<prefix>/bin/hya
<prefix>/bin/hya-backend
<prefix>/bin/hya-ts
<prefix>/lib/hya/hya-tui-ts/...
```

Direct `--bin-dir` installs derive the sibling `../lib/hya` directory and allow
an environment override for development. Rollback treats the three binaries
and runtime directory as one install unit.

CI adds pinned Bun install, typecheck, tests, boundary/branding/license audits,
and a package build before the Rust gates. Release packaging retains the
existing `hya` binary and adds the backend, `hya-ts`, and prepared TUI runtime
to the same versioned archive. This task changes packaging but does not publish
or tag a release.

## Rollback

- The current `hya` binary and Rust TUI are untouched and remain immediately
  runnable.
- Backend changes are isolated event-delivery/contract fixes with no schema or
  data migration.
- The new package and launcher can be removed without changing stored sessions.
- Installer rollback restores all previous binaries and the prior runtime
  directory together.
- If a retained screen requires OpenCode backend code, that screen is removed;
  the scope is not expanded silently.

## Risks And Gates

- Hidden backend imports: block with source and build-graph allowlists.
- SDK response drift: block each retained screen on a real-backend contract test.
- Terminal regression: retain upstream lifecycle tests and add a Linux PTY smoke.
- Orphaned backend: test owned versus attached process cleanup separately.
- Branding leakage: render snapshots plus a source audit with legal/protocol
  allowlists.
- Attribution omission: exact license/provenance archive assertions.
- Large imported diff: keep upstream and hya-owned code in separate directories,
  pin one commit, and add no speculative sync machinery.
