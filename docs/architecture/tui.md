# TUI Architecture

The shipped interactive frontend is the TypeScript/OpenTUI application under
[`../../packages/hya-tui-ts`](../../packages/hya-tui-ts). The Rust `hya`
binary is only its canonical Unix entrypoint.

This document covers **process ownership, contracts, and package boundaries**.
User-facing screens, transcript, dialogs, and prompt behavior live in
[TUI Reference](../tui-reference.md). Keybindings and slash commands live in
[TUI Keybindings](../tui-keybindings.md).

## Process Chain

```text
hya
  -> exec adjacent hya-ts
  -> start or attach to hya-backend
  -> run packages/hya-tui-ts/src/main.tsx with Bun
  -> use @opencode-ai/sdk/v2 over HTTP/SSE
```

[`../../crates/hya/src/main.rs`](../../crates/hya/src/main.rs) resolves
`hya-ts` beside the current executable and replaces its own process with it.
There is no PATH lookup or Rust frontend fallback. `arg0` remains `hya`, so
canonical help and errors use the public product name.

[`../../crates/hya-ts`](../../crates/hya-ts) owns launcher argument parsing,
runtime and backend discovery, and terminal process-group handoff (below). It
either starts an owned local `hya-backend` through `hya-sdk`, or attaches to the
URL supplied by `--server`.

## Terminal Handoff

When `hya-ts` launches Bun for the TUI, it performs job-control handoff so
terminal signals reach the frontend rather than the launcher
([`crates/hya-ts/src/main.rs`](../../crates/hya-ts/src/main.rs)):

1. **Capture.** `TerminalState::capture` reads the current foreground process
   group (`tcgetpgrp(STDIN)`) and the current termios (`tcgetattr`). If either
   call fails with `ENOTTY` (stdin is not a TTY), capture returns `None` and
   handoff is **skipped** entirely — Bun inherits the existing terminal state.
2. **Spawn.** Bun is started with `process_group(0)` so it is the leader of its
   **own** process group.
3. **Handoff.** The launcher transfers terminal foreground ownership to that
   group (`tcsetpgrp`) and sends `SIGCONT` to the group, so a stopped child
   resumes under the TTY.
4. **Restore.** On every exit path — normal Bun exit, spawn failure, handoff
   failure, or launcher signal cleanup — the original foreground pgid and
   termios are restored (`tcsetpgrp` + `tcsetattr`). `TerminalState::Drop` also
   restores, so a panic path does not leave the shell with a wrong foreground
   group.

That is why Ctrl-Z / Ctrl-C are delivered to the TUI process group rather than
to `hya-ts`.

## Launcher → TUI argv contract

The Bun entrypoint (`packages/hya-tui-ts/src/main.tsx`) uses a **strict**
`node:util` `parseArgs` (`strict: true`). An unknown flag is a hard error.

Accepted flags:

| Flag | Role |
| --- | --- |
| `--url` | **Required.** Backend base URL (owned ephemeral backend, or the value of `--server`). Missing → throws `--url is required`. |
| `--project` | Project directory (realpath'd; default is the optional positional path or `process.cwd()`). |
| `--continue` | Resume most recently updated root session. |
| `--session <ID>` | Open or fork a specific session. |
| `--fork` | Fork the continued or named session. |
| `--prompt <TEXT>` | Seed prompt. |
| `--agent <NAME>` | Seed agent. |
| `--model <PROVIDER/MODEL>` | Seed model. |
| (positional) | Optional project path when `--project` is omitted. |

`hya-ts` builds exactly this argv when it execs Bun: always
`src/main.tsx --url <url> --project <canonical project>`, then optional
`--continue`, `--session`, `--fork`, `--prompt`, `--agent`, `--model`
([`crates/hya-ts/src/lib.rs`](../../crates/hya-ts/src/lib.rs) `build_bun_command_with_url`).
The two sides must be changed together. After parse, the entrypoint
`realpath`s the project directory and `chdir`s into it before rendering.

## Owned backend lifecycle

When `hya` is launched without `--server`, `hya-sdk` spawns an owned backend
([`crates/hya-sdk/src/server.rs`](../../crates/hya-sdk/src/server.rs)
`ServerHandle::spawn_hya_backend`):

| Step | Behavior |
| --- | --- |
| Argv | Exactly `hya-backend serve --bind 127.0.0.1:0`, plus `--db <path>` when `HYA_DB` resolves to a non-empty path. If `HYA_DB` is set to the empty string, `--db` is omitted (in-memory store). Unset `HYA_DB` defaults to `$XDG_STATE_HOME/hya/sessions.db` (or `~/.local/state/hya/sessions.db`). |
| Process group | Child uses `process_group(0)` and `kill_on_drop(true)`. |
| Readiness | SDK waits up to **180 seconds** for a stdout/stderr line matching `listening on http://…` / `https://…` (`parse_listen_url`). The backend prints `hya server listening on {url}` ([`crates/hya-backend/src/serve.rs`](../../crates/hya-backend/src/serve.rs)). That listen line is a **load-bearing contract**, not merely a log message. |
| Teardown | On drop: `SIGTERM` the whole process group (negative pid), poll ~1 s, then `SIGKILL` the group. |

## Runtime and backend resolution

### TUI runtime (`HYA_TUI_TS_DIR`)

Assets are searched in order
([`resolve_runtime_dir`](../../crates/hya-ts/src/lib.rs)):

1. `HYA_TUI_TS_DIR` override (if set and canonicalizable)
2. `<exe>/../lib/hya/hya-tui-ts` (installed layout)
3. `<workspace>/packages/hya-tui-ts` (checkout layout)

Failure prints:
`cannot locate hya-tui-ts; set HYA_TUI_TS_DIR or install it under ../lib/hya/hya-tui-ts`.

### Backend binary (`HYA_BACKEND_BIN`)

When `--server` is absent, the launcher resolves `hya-backend` in order
([`resolve_backend_bin`](../../crates/hya-ts/src/lib.rs)):

1. `--backend-bin` flag
2. `HYA_BACKEND_BIN` environment variable
3. Sibling of the current executable named `hya-backend`
4. `target/release/hya-backend`, then `target/debug/hya-backend` under the workspace root
5. Fallback name `hya-backend` on `PATH`

A successful auto-spawn emits a `backend_spawn` startup mark (when
`HYA_STARTUP_TRACE` is truthy), then `backend_listen` with the discovered URL.

In a workspace checkout, developers usually hit (3) or (4). In an installed
layout, (2)/(3) apply under `bin/` with the TUI under `lib/hya/hya-tui-ts/`.

## Frontend Environment Flags

Truthy `HyaFlag` helpers accept **only** `1` or `true` (case-insensitive);
anything else is off
([`packages/hya-tui-ts/src/hya/platform.ts`](../../packages/hya-tui-ts/src/hya/platform.ts)).

| Variable | Meaning | Default |
| --- | --- | --- |
| `HYA_DISABLE_MOUSE` | Disable mouse capture | off |
| `HYA_DISABLE_TERMINAL_TITLE` | Stop the TUI setting the terminal title | off |
| `HYA_DISABLE_COPY_ON_SELECT` | Disable copy-on-select | off; **forced on for win32** regardless of the variable |
| `HYA_SHOW_TTFD` | Show time-to-first-draw | off |
| `HYA_WAIT_THEME` | Wait up to 1 s for the terminal theme reply before first paint | off (paint instantly in dark) |
| `HYA_SYNC_PLUGIN_START` | Gate shell routes on sequential builtin plugin-host start | off (paint shell immediately; `shell_paint=immediate`) |
| `HYA_VERSION` | Version string reported by the TUI | `"local"` |
| `HYA_CHANNEL` | Release channel reported by the TUI | `"local"` |
| `HYA_ROUTE` | JSON-encoded initial TUI route (`JSON.parse` at startup) | none — **not** a `HyaFlag` |
| `HYA_FAST_BOOT` | Skip the initial loading screen | off; **any non-empty value is on** (`Boolean(process.env.HYA_FAST_BOOT)`, so `0` counts as on) |
| `VISUAL` / `EDITOR` | External editor for long-prompt editing (`VISUAL` first, then `EDITOR`) | unset → open-editor no-ops |
| `XDG_CACHE_HOME` (and related) | TS-side paths: cache is `$XDG_CACHE_HOME/hya` or `~/.cache/hya`; data/config/state similarly under XDG or home defaults | independent of the Rust backend path resolution |

The TypeScript TUI computes data/cache/config/state directories in
`platform.ts` independently of Rust (`XDG_STATE_HOME`, etc.). Overriding XDG
variables for one process does not necessarily move the other process's files.

### Vendored upstream editor integration

These are not hya-configured product settings; they are upstream
editor-integration probes retained in the vendored frontend:

| Variable | Role |
| --- | --- |
| `CLAUDE_CODE_SSE_PORT` | IDE SSE port; checked **first** |
| `OPENCODE_EDITOR_SSE_PORT` | Fallback IDE SSE port |
| `OPENCODE_ZED_DB` | Override Zed database path |
| `ZED_TERM` / `TERM_PROGRAM` | Detect running inside a Zed terminal (`ZED_TERM=true` or `TERM_PROGRAM=zed`) |

## Frontend Ownership

The TypeScript package owns terminal rendering and interaction:

- SolidJS/OpenTUI application state and routes
- prompt, transcript, dialogs, themes, keybindings, and command palette
- session, model, agent, MCP, permission, and question views
- SDK HTTP calls and SSE synchronization
- static builtin plugin host (below)

The package is frontend-only. Provider execution, tools, permissions, events,
and persistence remain in `hya-backend` and its Rust library dependencies. The
frontend consumes the Compat-shaped SDK surface instead of constructing a
second runtime or projection.

User-visible chrome is described in [TUI Reference](../tui-reference.md) and
[TUI Keybindings](../tui-keybindings.md).

## Sessions and Startup

Public launcher flags include `--continue`, `--session <ID>`, and `--fork`.
`--fork` requires either `--continue` or `--session` (enforced in `hya-ts`).
Bare `hya-backend --resume <ID>` remains supported and launches
`hya --session <ID>`.

The removed Rust frontend options `--db`, `--yolo`, `--http`, `--compat`, and
`--resume` are not part of the public `hya` launcher.

### Startup navigation sequence

Inside the TUI (`app.tsx` onMount / effects):

1. **`--agent` / `--model`** seed the local agent and model. An invalid model
   format (not `provider/model`) raises a **3 s warning toast** rather than
   failing the process.
2. **`--session` without `--fork`** navigates to that session immediately.
3. **`--continue`** (optionally with `--fork`) runs as soon as sync is no longer
   `loading` (including **`partial`**): it picks the most recently updated
   **root** session (`parentID === undefined`). With `--fork`, it forks that
   session at that point.
4. **`--session` with `--fork`** deliberately waits for sync status
   **`complete`** before forking, so session-list reconcile cannot clobber the
   newly created session.

Internal entry (`bun src/main.tsx`) requires `--url` and accepts the argv set
above; it realpaths the project and chdirs into it before rendering — that is
what the Rust launcher execs.

## Subagent Workspace

Pane model
([`subagent-workspace.ts`](../../packages/hya-tui-ts/src/upstream/routes/session/subagent-workspace.ts)):

- Leaves are `MainPane` (`id: "main"`, uncloseable) or `ObservationPane`
  (`observation:<sessionID>`).
- `SplitPane` holds two child panes on a vertical or horizontal axis.
- Tabs are `WorkspaceTab[]` with a stable `observationOrder`, plus
  `activeTabID` and `focusedPaneID`.

The pure reducer is `reduceWorkspace` with actions: `close`, `openTab`,
`openSplit`, `focus`, `focusMain`, `cycleFocus`, `reconcileSessions`.

- **`reconcileSessions`** prunes observation panes whose session IDs are absent
  from the supplied set. Callers only dispatch it from a **successful** run-tree
  parse (`onTree`), never from a failed fetch, so a transient tree error does not
  wipe open panes.
- **`openSplitBesideMain`** implements ADR-0003's structural invariant: an
  observation is never nested inside another observation; the main tab is always
  `Main | observation`; other open observations are retained as separate tabs;
  focusing another open observation while split **promotes** it beside Main.

Exported seam `focusMainPromptOwnership` (session route) dispatches `focusMain`
and refocuses the prompt only when no modal is open.

Keybinding details for pane cycling and jumps live in
[TUI Keybindings](../tui-keybindings.md); this architecture doc does not
duplicate the binding tables.

### Run tree data contract

- **Endpoint.** The session route polls `GET /session/{id}/tree` through the raw
  SDK `fetch`. A non-ok HTTP response surfaces the error path; the UI text is
  `Subagent tree unavailable - press r to retry`.
- **Loader.** Generation-guarded: keeps the last valid tree on failure; allows
  one in-flight request plus one trailing refresh (`queued`); ignores stale
  responses; on a successful tree whose root `session` differs from the route,
  re-navigates to that root.
- **Schema.** Strictly validated recursive `RunTreeNode`: `session` / `agent` /
  `model` / `title`, optional `member` (`member`, `child`, `subagent_type`,
  `description`, `depth`, `status` in
  `spawning|running|done|failed|cancelled`, `summary`), optional `roster`
  (`handle`, `session`, `agent_type`, `mode` in `transient|resident`, `status`
  in `idle|busy|done|failed`, `current_task`), and `children[]`. Parse failures
  raise `RunTreeParseError` with a JSON path.
- **Invalidation.** A refresh is triggered by SSE/event payloads classified by
  `runTreeEventEffect`: `session.created|updated|deleted`, and `hya.envelope`
  events of type `member_spawned`, `member_status_changed`, `member_finished`,
  `agent_registered`, `agent_activity_changed` — each shape-validated before it
  counts.

### Task presentation seam

[`task-presentation.ts`](../../packages/hya-tui-ts/src/upstream/routes/session/task-presentation.ts)
is the stable internal contract Track T tests drive:

- `resolveTaskMembers` expands multi-member task metadata, then tool output,
  then falls back to `input.members` (useful while the call is still running),
  then a single top-level task row.
- `resolveTaskSessionId` prefers an explicit member session id, else matches the
  run tree by description (and type when needed).
- `launchedMembersFromTree` yields tree members not already covered by a task
  part, so live status still appears in the main message.

### Agent visibility

([`agent-visibility.ts`](../../packages/hya-tui-ts/src/upstream/util/agent-visibility.ts)):

- `isTuiSelectableAgent` admits only agents with `mode === "primary"` (Bundle
  role `main`) into the agent picker and `agent.cycle` rotation. **`hidden` is
  not a second selector rule** — a primary agent with `hidden` still appears.
- `isSubagentAutocompleteAgent` admits non-primary agents that are not
  wire-`hidden` (wire `hidden` encodes `can_spawn` reachability from the
  catalog). A non-primary agent must be non-hidden to be `@`-mentionable.

## Dialog primitives

Shared contributor seams (do not invent a second overlay):

- **`DialogSelect`**
  ([`dialog-select.tsx`](../../packages/hya-tui-ts/src/upstream/ui/dialog-select.tsx)):
  category grouping; `flat` while filtering; `filterActivation: immediate|slash`;
  `skipFilter` / `renderFilter`; `retainDisabled`; `current` marker; per-option
  gutter (e.g. spinners) and footers/details; action bar cycled with **Tab** /
  **Shift+Tab** binding to named commands.
- **Dialog container**
  ([`dialog.tsx`](../../packages/hya-tui-ts/src/upstream/ui/dialog.tsx)): modal
  stack with sizes `medium` (default, 60 columns), `large` (88), `xlarge` (116);
  pushes keymap mode `modal`; Escape / Ctrl+C close the top entry after first
  clearing any text selection; `replace` / `clear` reset size to `medium`.

## Frontend Plugin Host

This is **unrelated** to the backend `hya-plugin` stdio host.

hya replaces the upstream dynamic plugin loader with a **static host**
([`static-host.ts`](../../packages/hya-tui-ts/src/hya/static-host.ts)): starts
all builtin plugins in parallel, tracks cleanups, reports statuses in stable
declaration order. There is no external plugin manager and no dynamic loading.

Builtin ids in declaration order (eleven):

1. `internal:home-footer`
2. `internal:home-tips`
3. `internal:sidebar-context`
4. `internal:sidebar-mcp`
5. `internal:sidebar-lsp`
6. `internal:sidebar-todo`
7. `internal:sidebar-files`
8. `internal:sidebar-footer`
9. `internal:notifications`
10. `which-key`
11. `diff-viewer`

Render-extension slots used by the shell and builtins:

| Slot | Mode notes |
| --- | --- |
| `app`, `app_bottom` | App chrome |
| `home_logo`, `home_prompt` | `replace` |
| `home_prompt_right`, `home_bottom` | Home chrome |
| `home_footer` | `single_winner` |
| `sidebar_title`, `sidebar_footer` | `single_winner` |
| `sidebar_content` | Sidebar body |
| `session_prompt`, `session_prompt_right` | Session prompt chrome |

Plugin API surface (lifetime-tracked by the static host where registration
returns disposers): `app` (version), `state` (session / provider / mcp / lsp /
config / path / vcs), `theme`, `keys`, `keymap` (including `registerLayer`),
`route.register` / `navigate` / `current`, `event.on`, `kv`, `ui.dialog` (and
related dialog helpers), `attention`, `renderer`, `tuiConfig`, `slots.register`.

`HYA_SYNC_PLUGIN_START` gates shell routes on sequential start (classic mode).
Default paints shell chrome immediately and marks `shell_paint=immediate`.

## Renderer

OpenTUI renderer configuration
([`app.tsx`](../../packages/hya-tui-ts/src/upstream/app.tsx)):

- `targetFps: 60`
- `externalOutputMode: "passthrough"`
- Kitty keyboard protocol (`useKittyKeyboard: {}`)
- `autoFocus: false`
- `exitOnCtrlC: false` — Ctrl+C is routed through the keymap (copy selection /
  clear prompt / dialog dismiss) rather than exiting the process
- `openConsoleOnError: false`
- SIGHUP handler tears the renderer down cleanly

Windows shims: `win32DisableProcessedInput` on start and
`win32FlushInputBuffer` on exit keep Windows consoles from swallowing or
replaying keys. `terminal.suspend` (ctrl+z) is **disabled on win32**, where
ctrl+z folds into `input.undo` instead.

## Backend-driven control events

The TypeScript frontend consumes these events in the main app loop (each
`tui.*` handler is ignored unless the event's workspace matches the current
workspace):

| Event | Effect |
| --- | --- |
| `tui.command.execute` | Dispatches a keymap command by name |
| `tui.toast.show` | Toast with `title` / `message` / `variant` / `duration` |
| `tui.session.select` | Navigates to the given session |
| `session.deleted` | If the open session was deleted → navigate home with toast `The current session was deleted` |
| `session.error` | 5 s error toast unless the error name is `MessageAbortedError` |

## Enforced Boundaries

Contributors learn these from tests before a failure is the first signal.

### Excluded upstream surface

[`branding-pruning.test.ts`](../../packages/hya-tui-ts/test/branding-pruning.test.ts)
greps all package source so these stay out: `docs.open`, `provider.connect`,
`session.share` / `unshare`, `workspace.list` / `set` / `create` / `remove` /
`warp` / `adapter`, `console.org.switch`, `plugins.list` / `install`,
`global.upgrade`, `dialog-provider`, `DialogWorkspace`, `DialogRetryAction`,
and related console/share paths.

### Import / path / dependency pins

[`boundary.test.ts`](../../packages/hya-tui-ts/test/boundary.test.ts):

- Forbidden path/import regexes ban `backend|server|provider|worker|updater|console`
  modules and `@opencode-ai/{core,ui,provider}` imports.
- Every runtime dependency version is pinned by the boundary test (currently
  including `@opencode-ai/plugin` / `@opencode-ai/sdk` at `1.17.9` and the
  OpenTUI packages at `0.3.4`).

### Rust-side guardrail

[`crates/hya/tests/no_rust_tui.rs`](../../crates/hya/tests/no_rust_tui.rs)
asserts that `crates/hya-tui`, `crates/hya-tui-lib`, and `crates/hya-parity` do
not exist and that no Cargo manifest references them.

Intent: the TypeScript package is frontend-only and must consume the
Compat-shaped SDK rather than construct a second runtime.

## Package exports

Maintainer-facing surface of `packages/hya-tui-ts`:

| Export | Location | Role |
| --- | --- | --- |
| `run`, type `TuiInput` | `upstream/index.tsx` → `app.tsx` | Upstream barrel. `TuiInput` is `{ url, args, config, onSnapshot?, directory?, fetch?, headers?, events?, pluginHost }`. |
| `launch(argv, runner?)` | `main.tsx` | Injectable entry tests drive; default runner provides `HyaPlatform` and runs the Effect program. |
| `HyaPaths`, `HyaPlatform`, `HyaFlag`, `HyaVersion`, `HyaChannel` | `hya/platform.ts` | Paths + Effect service + env flags + version channel. |
| `PRODUCT_NAME`, `STATUS_COMMAND`, `DEFAULT_THEME`, `DEFAULT_SOUND_PACK`, `CLIPBOARD_TEMP_NAME`, `terminalTitle()` | `hya/product.ts` | Product constants (`"hya"`, `"hya.status"`, `"hya"`, `"hya.default"`, `"hya-clipboard.png"`). |
| `auditSurface` | `hya/audit.ts` | Freezes branded presentation map, terminal title, default theme/sound pack, XDG paths, temp name, builtin plugin ids, and `hya.status` for branding tests. |
| `startupMark`, `startupTraceEnabled` | `hya/startup-trace.ts` | Structured startup marks when `HYA_STARTUP_TRACE` is truthy. |
| `createStaticPluginHost()` | `hya/static-host.ts` | Returns `TuiPluginHost`. |
| `observeSdkSpine(input, ready)` | `hya/sdk-spine.tsx` | Headless SDK/sync/data provider chain; resolves when `ready` passes; rejects after **5 s** with `SDK spine timed out`. **Test seam.** |

`auditSurface`, `observeSdkSpine`, and `launch`'s injectable runner exist
primarily so branding and Track T tests can assert without a full interactive
terminal.

## Sole Frontend Implementation

`packages/hya-tui-ts` is the only interactive terminal UI implementation in
this repository. There is no retained Rust TUI crate. New interactive behavior
belongs there; reusable protocol behavior belongs below the HTTP/SDK boundary.

`hya-backend` may launch the current `hya` frontend for bare interactive
startup, but it does not own a terminal renderer.

## Installation Contract

The frontend is a colocated installation, not a standalone Cargo binary. The
supported installer and release archives contain:

```text
bin/hya
bin/hya-ts
bin/hya-backend
lib/hya/hya-tui-ts/
```

Bun must be available when installing and running hya. The installer prepares
production dependencies with the pinned lockfile and removes SDK server code
that the frontend does not use.
