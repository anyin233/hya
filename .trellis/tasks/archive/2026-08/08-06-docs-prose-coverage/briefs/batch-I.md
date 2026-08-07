# Batch I - tui.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/architecture/tui.md`

You have **19 gap entries** and **0 stale claims** to resolve.

This file is ARCHITECTURE only. User-facing TUI behaviour now lives in docs/tui-reference.md and docs/tui-keybindings.md, which already exist -- link to them rather than repeating their content.

## Non-negotiable rules

1. **Confirm every claim against the source before you write it.** Every entry
   below carries a `source` reference. Open it. If the source contradicts the
   entry, the SOURCE WINS -- write what the code does and report the discrepancy.
2. **If you cannot confirm a claim from source, do not write it.** Say you could
   not confirm it. Plausible prose that is wrong is worse than an admitted gap,
   because a reader trusts the document.
3. **Stale and contradicted entries are corrected or deleted, never merely
   supplemented.** A document that contradicts the code is a defect.
4. **Do not edit any file outside your batch.** Other writers are working in
   parallel. In particular never touch `docs/README.md`, `README.md`, `AGENTS.md`,
   `DESIGN.md`, or `docs/project-structure.md` -- a later reconciliation pass owns
   all cross-links and the docs map. Some entries below suggest edits to other
   files; ignore that part and write only your own.
5. **Match the existing documentation style.** Read the file you are editing
   before writing. Use the project's vocabulary as defined in `CONTEXT.md`.
6. **A feature counts as documented only if a reader can use it** from what you
   write: what it does, its parameters or keys, and its semantics. A name in a
   list does not count. 4 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/architecture/tui.md`

**1. Terminal handoff / job control performed by hya-ts** — `thin` · severity low

- Source: `crates/hya-ts/src/main.rs:198`
- Evidence: docs/architecture/tui.md:23-24 lists 'terminal process-group handoff, signal cleanup, and terminal restoration' as a bullet fragment. Grep for tcgetpgrp and termios returns zero hits. Nothing documents the SIGCONT step, the always-restore guarantee, or the ENOTTY skip — all of which matter when hya is run under a non-TTY stdin or inside another supervisor.
- Write: Expand the hya-ts responsibilities paragraph into a '## Terminal Handoff' subsection describing the actual sequence: hya-ts captures the current foreground process group (tcgetpgrp) and termios settings, spawns Bun in its OWN process group, transfers terminal foreground ownership to that group, sends SIGCONT, and on every exit path — normal exit, error, or signal — restores the original foreground pgid and termios. If stdin is not a TTY (tcgetpgrp returns ENOTTY), handoff is skipped entirely and Bun inherits the terminal state as-is. Note that this is why Ctrl-Z/Ctrl-C reach the TUI rather than the launcher. Source crates/hya-ts/src/main.rs.

**2. TUI argv contract between hya-ts and the Bun entrypoint (--url required, --project, --continue, --session, --fork, --prompt, --agent, --model, positional path)** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/main.tsx:13; crates/hya-ts/src/lib.rs:323`
- Evidence: docs/architecture/tui.md documents the process chain down to 'run packages/hya-tui-ts/src/main.tsx with Bun' but never states the argv contract. No in-scope doc mentions --url.
- Write: Add a '## Launcher → TUI argv contract' subsection. The Bun entrypoint uses a STRICT node:util parseArgs, so an unknown flag is a hard error. Accepted: --url (required; throws '--url is required' when absent — this is the backend base URL, either the owned ephemeral backend or the value of --server), --project (canonicalized project directory; hya-ts canonicalizes the optional positional PROJECT, defaulting to the process cwd), --continue, --session <ID>, --fork, --prompt <TEXT>, --agent <NAME>, --model <PROVIDER/MODEL>, plus one positional project path. State that this is exactly the argv hya-ts constructs (crates/hya-ts/src/lib.rs:323) and that the two sides must be changed together.

**3. Auto-spawned backend argv and lifecycle (hya-backend serve --bind 127.0.0.1:0 [--db …], 180s listen wait, process-group kill on drop)** — `thin` · severity medium

- Source: `crates/hya-sdk/src/server.rs:73,118`
- Evidence: docs/cli.md:70-72 documents only the resulting DB path; docs/architecture/tui.md:26 says only 'starts an owned local hya-backend through hya-sdk'. The exact argv, the 180-second startup timeout, the reliance on parsing the 'hya server listening on http://<addr>' stdout line, and the SIGTERM-then-SIGKILL process-group teardown are undocumented — all of which a user debugging a hung or orphaned backend needs.
- Write: Add a '## Owned backend lifecycle' subsection. When `hya` is launched without --server, hya-sdk spawns exactly `hya-backend serve --bind 127.0.0.1:0` plus `--db <path>` when HYA_DB resolves to a non-empty path (the --db flag is omitted entirely when HYA_DB is set to the empty string, giving an in-memory store). The child is placed in its own process group with kill_on_drop. The SDK discovers the base URL by parsing the backend's stdout line `hya server listening on http://<addr>` and waits up to 180 seconds for it; on drop it SIGTERMs and then SIGKILLs the whole process group. Note that the listen line is a load-bearing contract, not just a log message, so its format must not change. Sources crates/hya-sdk/src/server.rs and crates/hya-backend/src/serve.rs.

**4. TS TUI environment flags: HYA_DISABLE_MOUSE, HYA_DISABLE_TERMINAL_TITLE, HYA_DISABLE_COPY_ON_SELECT, HYA_SHOW_TTFD, HYA_WAIT_THEME, HYA_SYNC_PLUGIN_START, HYA_VERSION, HYA_CHANNEL, HYA_ROUTE, HYA_FAST_BOOT** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/hya/platform.ts:21-35; packages/hya-tui-ts/src/upstream/app.tsx:249-250`
- Evidence: grep for HYA_DISABLE, HYA_FAST_BOOT, HYA_ROUTE, HYA_VERSION, HYA_CHANNEL, HYA_SHOW_TTFD, HYA_WAIT_THEME, HYA_SYNC_PLUGIN_START across all in-scope docs returns zero hits for every one of them. docs/architecture/tui.md mentions only HYA_TUI_TS_DIR / HYA_BACKEND_BIN.
- Write: Add a `## Frontend Environment Flags` table to docs/architecture/tui.md covering the TS TUI variables, and note at the top that the HyaFlag helper treats ONLY `1` or `true` (case-insensitive) as truthy, everything else as off. Rows (platform.ts:21-35): HYA_DISABLE_MOUSE — disable mouse capture, default off; HYA_DISABLE_TERMINAL_TITLE — stop the TUI setting the terminal title, default off; HYA_DISABLE_COPY_ON_SELECT — disable copy-on-select, default off but FORCED ON for win32 regardless of the variable; HYA_SHOW_TTFD — show time-to-first-draw, default off; HYA_WAIT_THEME — wait up to 1 s for the terminal theme reply before the first paint, default off (paint instantly in dark); HYA_SYNC_PLUGIN_START — gate shell routes on sequential builtin plugin-host start, default off (paint the shell immediately); HYA_VERSION — version string reported by the TUI, default "local"; HYA_CHANNEL — release channel reported by the TUI, default "local". Plus two that do NOT use the HyaFlag helper (app.tsx:249-250): HYA_ROUTE — a JSON-encoded initial TUI route, JSON.parse'd at startup, no default; HYA_FAST_BOOT — ANY non-empty value (plain Boolean coercion, so `0` counts as on) skips the initial loading screen.

**5. XDG_CACHE_HOME (TypeScript side paths)** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/hya/platform.ts:7, 11-17`
- Evidence: grep for `XDG_CACHE_HOME` across all in-scope docs returns zero hits (XDG_STATE_HOME and XDG_DATA_HOME are covered in docs/cli.md:71 and :102).
- Write: In the new Frontend Environment Flags section note that the TypeScript TUI computes its own data/cache/config/state directories independently of the Rust side (platform.ts:7,11-17): the cache dir is `$XDG_CACHE_HOME/hya`, falling back to `~/.cache/hya`. Warn that because the two sides resolve paths separately, overriding XDG variables for one process does not necessarily move the other's files.

**6. VISUAL / EDITOR** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/editor.ts:27`
- Evidence: grep for `VISUAL` and `EDITOR` across all in-scope docs returns zero hits.
- Write: Add a row to the TUI environment table: the external editor the TUI opens for long-prompt editing is taken from `VISUAL` first, then `EDITOR` (packages/hya-tui-ts/src/upstream/editor.ts:27). Mention this in docs/troubleshooting.md too if the editor fails to open.

**7. Editor-integration detection: CLAUDE_CODE_SSE_PORT, OPENCODE_EDITOR_SSE_PORT, OPENCODE_ZED_DB, ZED_TERM, TERM_PROGRAM** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/upstream/context/editor.ts:117-121; packages/hya-tui-ts/src/upstream/editor-zed.ts:189, 198`
- Evidence: grep for each of these names across all in-scope docs returns zero hits.
- Write: In the TUI environment table, add a short vendored-upstream group: `CLAUDE_CODE_SSE_PORT` and `OPENCODE_EDITOR_SSE_PORT` supply the SSE port of an attached IDE, with the Claude Code variable checked FIRST (context/editor.ts:117-121); `OPENCODE_ZED_DB` overrides the Zed database path and `ZED_TERM` / `TERM_PROGRAM` are used to detect that the TUI is running inside a Zed terminal (editor-zed.ts:189,198). Mark the group as vendored upstream editor-integration behavior that hya does not otherwise configure.

**8. Subagent workspace pane model, actions, and the split-beside-Main invariant** — `stale` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/routes/session/subagent-workspace.ts:237`
- Evidence: docs/adr/0003 describes the design but at the level of the removed Rust TUI: it says 'Navigation initially reuses existing observation controls: Ctrl+X . cycles focus, Ctrl+X W closes… No dedicated tab-next/tab-prev bindings are introduced', while the shipped code adds `<leader>left`/`<leader>right` cycling and digit 1-9 pane jump; it also promises a 'new-output indicator' on manually-scrolled observation views that the implementation does not render. No doc describes the actual pane tree, the reducer, or `reconcileSessions`.
- Write: Add a '## Subagent Workspace' section. Describe the pane model: a tree of MainPane / ObservationPane / SplitPane grouped into WorkspaceTabs, with a stable `observationOrder`, an `activeTabID` and a `focusedPaneID`; Main is uncloseable. Describe the pure reducer `reduceWorkspace` and its actions — close, openTab, openSplit, focus, focusMain, cycleFocus, reconcileSessions — noting that `reconcileSessions` prunes observation panes whose session vanished from a *successful* run-tree fetch only. State ADR-0003's structural invariant as implemented by `openSplitBesideMain`: an observation is never nested inside another observation, the main tab is always `Main | observation`, previously open observations are retained as separate tabs, and focusing another open observation while split promotes it beside Main. Document the exported seam `focusMainPromptOwnership`, which dispatches focusMain and refocuses the prompt only when no modal is open. Then update docs/adr/0003 to drop the two stale consequences (no-new-bindings, new-output indicator) and point at this section.

**9. Subagent run tree: GET /session/{id}/tree, loader semantics, invalidation events, and the RunTreeNode/RunTreeMember/RosterEntry schema** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/routes/session/subagent-workspace.ts:4`
- Evidence: docs/architecture/server-client.md lists route groups but grep for 'session/.*/tree', 'RunTree', 'run tree' across all in-scope docs returns nothing. docs/architecture/event-model.md does not mention the TUI's tree-invalidation event set.
- Write: In the Subagent Workspace section, document the data contract. Endpoint: the session route polls `GET /session/{id}/tree` through the raw SDK fetch; a non-ok response surfaces 'Subagent tree unavailable - press r to retry'. Loader semantics: generation-guarded, keeps the last valid tree on failure, allows one in-flight request plus one trailing refresh, ignores stale responses, and re-navigates the route when the tree root session differs from the current one. Schema: a strictly validated recursive payload where each node carries session/agent/model/title, an optional member (member, child, subagent_type, description, depth, status in spawning|running|done|failed|cancelled, summary) and an optional roster entry (handle, session, agent_type, mode in transient|resident, status in idle|busy|done|failed, current_task); parse failures raise `RunTreeParseError` carrying a JSON path. Invalidation: a refresh is triggered by `session.created|updated|deleted` and by `hya.envelope` events of type `member_spawned`, `member_status_changed`, `member_finished`, `agent_registered`, `agent_activity_changed`, each shape-validated before it counts.

**10. Task presentation helpers (resolveTaskMembers, resolveTaskSessionId, launchedMembersFromTree)** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/upstream/routes/session/task-presentation.ts:1`
- Evidence: docs/testing/agent-matrix.md:86 names the test file `task-presentation.test.ts` but no doc explains what the module does. No source comments are referenced by any doc.
- Write: In the Subagent Workspace section, add a short paragraph on the presentation seam: `resolveTaskMembers` expands multi-member task metadata, falling back to the single task input and, while the call is still running, to `input.members`; `resolveTaskSessionId` matches a member to a run-tree session by description; `launchedMembersFromTree` yields tree members not already covered by a task part. Explain that this is the seam Track T tests drive so writers know it is a stable internal contract, not an implementation detail.

**11. Agent selector and subagent autocomplete visibility rules (isTuiSelectableAgent / isSubagentAutocompleteAgent)** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/util/agent-visibility.ts:5`
- Evidence: docs/configuration.md and docs/agent-bundle-authoring.md describe agent modes but no doc states which agents reach the TUI agent picker versus the `@` autocomplete. grep for 'isTuiSelectableAgent', 'selectable agent', 'agent picker' returns nothing.
- Write: Add an 'Agent visibility' subsection stating the two rules exactly: `isTuiSelectableAgent` admits only agents with `mode === "primary"` (Bundle role `main`) into the agent picker and the `agent.cycle` rotation, and `hidden` is explicitly NOT a second selector rule; `isSubagentAutocompleteAgent` admits non-primary agents that are not wire-`hidden`, encoding can_spawn reachability from the catalog. Say why this matters for bundle authors: setting `hidden` on a primary agent does not remove it from the picker, and a non-primary agent must be non-hidden to be `@`-mentionable. Cross-link from docs/agent-bundle-authoring.md.

**12. Generic picker widget DialogSelect and the dialog container (sizes, modal keymap mode, Escape/Ctrl+C behavior)** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/ui/dialog-select.tsx:30`
- Evidence: No in-scope doc describes the shared dialog primitives. This is the seam every new dialog is built on and there is no guidance for contributors adding one.
- Write: Add a 'Dialog primitives' subsection for contributors. `DialogSelect` is the shared list widget: category grouping, `flat` flattening while filtering, `filterActivation: immediate|slash`, `skipFilter`/`renderFilter`, `retainDisabled`, a `current` marker, gutter spinners, per-option footers/details, and an action bar cycled with Tab / Shift+Tab whose entries bind to named commands. The dialog container is a modal stack with sizes medium (default), large (88 columns) and xlarge (116 columns); it pushes a `modal` keymap mode, closes the top entry on Escape or Ctrl+C (first clearing any text selection), and `replace`/`clear` reset the size back to medium. Say plainly that new dialogs should use these rather than rendering their own overlay.

**13. Static plugin host, builtin plugin ids, plugin slots, and the TUI plugin API surface** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/hya/static-host.ts:6`
- Evidence: No in-scope doc mentions the TUI plugin host. docs/architecture/tui.md says only that the package 'owns terminal rendering and interaction'. crates/hya-plugin docs cover the backend plugin host, which is a different thing entirely — a reader could easily confuse the two.
- Write: Add a '## Frontend Plugin Host' section, stating up front that this is unrelated to the backend `hya-plugin` stdio host. hya replaces the upstream dynamic plugin loader with a static host that starts all builtin plugins in parallel, tracks their cleanups, and reports statuses in stable declaration order; there is no external plugin manager and no dynamic loading. List the exactly eleven builtin ids in declaration order: internal:home-footer, internal:home-tips, internal:sidebar-context, internal:sidebar-mcp, internal:sidebar-lsp, internal:sidebar-todo, internal:sidebar-files, internal:sidebar-footer, internal:notifications, which-key, diff-viewer. List the render-extension slot names used by the shell and builtins — app, app_bottom, home_logo, home_prompt, home_prompt_right, home_bottom, home_footer, sidebar_title, sidebar_content, sidebar_footer, session_prompt, session_prompt_right — and their `replace` and `single_winner` modes. List the API surface a plugin receives: app (version), state (session/provider/mcp/lsp/config/path/vcs), theme, keys, keymap.registerLayer, route.register/navigate/current, event.on, kv, ui.dialog, attention.notify, renderer, tuiConfig, slots.register — all lifetime-tracked by the static host. Note `HYA_SYNC_PLUGIN_START` gates the shell routes on sequential start (classic mode); the default paints shell chrome immediately and marks `shell_paint=immediate`.

**14. Renderer configuration and win32 terminal shims** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/app.tsx:154`
- Evidence: No in-scope doc mentions the OpenTUI renderer settings, the SIGHUP handler, or the Windows console shims.
- Write: Add a short 'Renderer' subsection: the OpenTUI renderer runs at 60 fps with passthrough external output, the Kitty keyboard protocol, no auto-focus, `exitOnCtrlC: false` (Ctrl+C is routed through the keymap so it can copy a selection or clear the prompt instead of exiting), console errors not auto-opened, and a SIGHUP handler that tears the renderer down cleanly. Note the Windows shims `win32DisableProcessedInput` on start and `win32FlushInputBuffer` on exit, which keep Windows consoles from swallowing or replaying keys, and that `terminal.suspend` (ctrl+z) is disabled on win32 where ctrl+z folds into `input.undo` instead.

**15. Backend-driven TUI control events (tui.command.execute, tui.toast.show, tui.session.select) and session.deleted / session.error handling** — `contradicted` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/app.tsx:855`
- Evidence: docs/compat-parity.md:116 says of the `/tui/*` routes: 'Missing real TUI main-loop integration and event-bus delivery parity.' The TypeScript frontend does handle these events in app.tsx — tui.command.execute dispatches a keymap command, tui.toast.show raises a toast, tui.session.select navigates — each gated on a workspace match.
- Write: Add a 'Backend-driven control events' subsection documenting the three events the frontend consumes — `tui.command.execute` (dispatches a keymap command by name), `tui.toast.show` (title/message/variant/duration), `tui.session.select` (navigates to a session) — and state the gate: each is ignored unless the event's workspace matches the current one. Also document `session.deleted` (deleting the open session navigates home with the toast 'The current session was deleted') and `session.error` (raises a 5 s error toast unless the error is `MessageAbortedError`). Then correct docs/compat-parity.md:116, which still claims there is no real main-loop integration.

**16. Startup navigation from CLI args (--agent, --model, --session, --continue, --fork sync gating)** — `thin` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/app.tsx:455`
- Evidence: docs/cli.md:51-56 lists the flags with one-line meanings and docs/architecture/tui.md:50-53 states the `--fork` requires `--continue`-or-`--session` rule. Neither documents the sync-phase gating that makes the behavior observable (why `--fork` is slower, why `--continue` can pick a session before the list looks complete) or the invalid-model toast.
- Write: Extend the 'Sessions and Startup' section with the actual startup sequence: `--agent` and `--model` seed the local agent and model, and an invalid model format raises a 3 s warning toast rather than failing; `--session` without `--fork` navigates directly; `--continue` picks the most recently updated root session as soon as sync reaches `partial`; `--fork` deliberately waits for sync `complete` before forking so reconcile cannot clobber the newly created session. Note the internal entry contract as well: `bun src/main.tsx` requires `--url` and additionally accepts `--project`, `--continue`, `--session`, `--fork`, `--prompt`, `--agent`, `--model`, resolves the project realpath and chdirs into it before rendering — this is what the Rust launcher execs.

**17. TUI runtime dir resolution order (HYA_TUI_TS_DIR) and backend auto-spawn (HYA_BACKEND_BIN)** — `thin` · severity medium

- Source: `crates/hya-ts/src/lib.rs:355`
- Evidence: docs/architecture/tui.md:31 names both variables in one sentence ('provide explicit development or diagnostic overrides') without stating the search order, the fallback chain, or the error text. docs/troubleshooting.md:12 mentions the failure symptom but not how to diagnose it. Neither appears in the docs/configuration.md environment table.
- Write: Replace the one-line mention with the explicit resolution orders. Runtime assets are searched: (1) the `HYA_TUI_TS_DIR` override, (2) `<exe>/../lib/hya/hya-tui-ts`, (3) `packages/hya-tui-ts` in the workspace; failure prints an actionable error. Backend: when `--server` is absent the launcher resolves `hya-backend` in order — the `--backend-bin` flag, `HYA_BACKEND_BIN`, a sibling of the current executable, then `target/release` and `target/debug` — and spawns it, emitting a `backend_spawn` startup mark. Explain which order applies in a workspace checkout versus an installed layout, and cross-link the startup-trace troubleshooting section.

**18. Package boundary and pruning invariants (excluded upstream features, forbidden imports, pinned dependency versions)** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/test/branding-pruning.test.ts:45`
- Evidence: AGENTS.md:100 states the positive rule ('Put all new interactive terminal UI behavior in packages/hya-tui-ts. Do not reintroduce a Rust TUI crate') but nothing documents the enforced exclusion list or the import boundary. A contributor porting upstream code would only discover these by failing a test.
- Write: Add a '## Enforced Boundaries' section so contributors learn the rules before a test fails. Excluded upstream surface (a test greps all source to keep them out): docs.open, provider.connect, session.share/unshare, workspace.list/set/create/remove/warp/adapter, console.org.switch, plugins.list/install, global.upgrade, dialog-provider, DialogWorkspace, DialogRetryAction. Import/path boundary: forbidden path and import regexes ban any `backend|server|provider|worker|updater|console` module and any `@opencode-ai/{core,ui,provider}` import, and every runtime dependency version is pinned by the boundary test. Also record the Rust-side guardrail: a test asserts `crates/hya-tui`, `crates/hya-tui-lib` and `crates/hya-parity` do not exist and that no Cargo manifest references them. Explain the intent — the TypeScript package is frontend-only and must consume the Compat-shaped SDK rather than construct a second runtime.

**19. Package public exports (run/TuiInput, launch, HyaPlatform surface, product constants, auditSurface, startupMark, createStaticPluginHost, observeSdkSpine)** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/upstream/index.tsx:1`
- Evidence: No in-scope doc lists the package's exported API. docs/project-structure.md:212 gives one line about the package's purpose only.
- Write: Add a 'Package exports' table for maintainers: the upstream barrel exports exactly `run` and the `TuiInput` type, where `TuiInput` is `{ url, args, config, onSnapshot?, directory?, fetch?, headers?, events?, pluginHost }`; `launch(argv, runner?)` is the injectable entry the tests drive, with the default runner providing the `HyaPlatform` service and running the Effect program; the hya platform module exports `HyaPaths`, the `HyaPlatform` Effect service, the `HyaFlag` env-flag record, and `HyaVersion` / `HyaChannel`; the product module exports `PRODUCT_NAME="hya"`, `STATUS_COMMAND="hya.status"`, `DEFAULT_THEME="hya"`, `DEFAULT_SOUND_PACK="hya.default"`, `CLIPBOARD_TEMP_NAME="hya-clipboard.png"` and `terminalTitle()`; `auditSurface` freezes the branded presentation map, terminal title, default theme and sound pack, XDG paths, temp file name, builtin plugin ids and the `hya.status` command for the branding test to assert against; `startupMark(mark, detail?, { once })` and `startupTraceEnabled()`; `createStaticPluginHost(): TuiPluginHost`; and `observeSdkSpine(input, ready)`, which mounts only the SDK/sync/data provider chain headlessly and resolves when the predicate passes, rejecting after 5 s with 'SDK spine timed out'. Say which of these exist purely as test seams.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 19 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
