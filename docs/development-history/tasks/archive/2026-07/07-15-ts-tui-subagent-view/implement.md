# Implementation Plan

## Goal

Replace the TypeScript TUI's whole-route child-session viewer with one native,
read-only subagent workspace that renders the authoritative recursive run tree,
supports tabs and repeated splits, preserves one main Prompt, and verifies that a
child can spawn a visible grandchild.

Success is the conjunction of the observable acceptance contract below. The
implementation is incomplete if the replacement works but any legacy child
command/view remains reachable, input can target an observation session, or the
required Rust/Bun verification gate fails.

## Non-Goals

- Do not change `SpawnerPlane`, `spawn_team_supervisor`, `run_team`, governor
  limits, event storage, or transcript/copy/export semantics.
- Do not add persisted layout, polling, a frontend team projection, a second
  tree endpoint, a runtime dependency, a ratio editor, or split-size admission
  thresholds.
- Do not refactor unrelated Session/TUI code or overwrite pre-existing dirty
  hunks.
- Do not edit release metadata until its existing `0.33.3` ownership is resolved.

## Observable Acceptance Contract

- **AC-01:** `<leader>o` opens the native recursive manager; `Ctrl+X`, then
  `Down` no longer opens or navigates to a child viewer.
- **AC-02:** The manager renders root, children, pending member-only nodes, and
  grandchildren in depth-first parent/child order.
- **AC-03:** A task spawned from a child session appears beneath that child and
  can spawn its own child within existing governor policy.
- **AC-04:** An executable Rust cross-layer test drives the existing child-scoped
  spawner and observes root -> child -> grandchild through the HTTP tree route.
- **AC-05:** One tab can show at least two different observation transcripts at
  the same time, and both continue receiving existing sync updates.
- **AC-06:** Repeated vertical/horizontal splits preserve pane identity; focus or
  close changes only the targeted leaf and collapses only its empty parent.
- **AC-07:** An unfocused terminal observation closes immediately. A focused
  terminal observation shows final state until the next focus transition, then
  closes.
- **AC-08:** Auxiliary tabs can be created, switched, and closed while mounted
  pane transcript/scroll state remains intact; Main is unique and uncloseable.
- **AC-09:** The exact seven pane commands/defaults in Fixed Contracts are
  registered and their configured shortcuts appear in relevant hints.
- **AC-10:** In the manager, `Enter`, `v`, and `s` place the selected session as
  tab, vertical split, and horizontal split. Cancel, empty, root, pending, or
  stale selections leave layout unchanged.
- **AC-11:** Focusing any observation leaves the main Prompt draft and target
  unchanged; ordinary text and Enter produce no prompt request or event.
- **AC-12:** Opening an observed session focuses its existing leaf, never a
  duplicate. Changing main session clears all ephemeral workspace state.
- **AC-13:** Observation panes contain a read-only header/transcript and no
  Prompt, permission answer control, question answer control, or route action.
- **AC-14:** Legacy child/parent commands, keybinds, footer, task hint, and route
  navigation are absent and old keyboard paths cannot leave the root route.
- **AC-15:** A task row opens/focuses its session through the workspace callback
  and advertises the configured `pane.roster` shortcut, not the legacy hint.
- **AC-16:** Existing member/team/session projections drive status and lifecycle;
  stored transcripts remain the only copy/export source.
- **AC-17:** Focus, navigation, terminal cleanup, request sequencing, narrow/wide
  rendering, and read-only input are covered by the named Rust/Bun/PTy tests.
- **AC-18:** Formatting, clippy, workspace tests, TS typecheck/tests/build, local
  `hya-backend`/`hya-ts` build, Trellis validation, diff check, and status review
  all pass.

## Fixed Contracts

- Backend DTO: `crates/hya-proto/src/projection_tree.rs::RunTreeNode` gains
  `roster: Option<RosterEntry>`. `build_run_tree` receives a
  `HashMap<SessionId, RosterEntry>` index. Root and member-only nodes omit it.
- Backend endpoint: `crates/hya-server/src/compat/session_legacy_basic.rs::tree`
  keeps the root `Projection.team.roster`, indexes it by `RosterEntry.session`,
  and passes it to `build_run_tree`. B2B must first characterize that both a
  child and grandchild are registered in that root roster.
- Nested-spawn seam: `crates/hya-app/src/runtime.rs::spawn_team_supervisor` and
  `hya_tool::SpawnerPlane::for_session`; no alternate spawn mechanism is allowed.
- New product code: exactly one new module,
  `packages/hya-tui-ts/src/upstream/routes/session/subagent-workspace.ts`.
- TS integration: `index.tsx::Session`, `index.tsx::Task`,
  `dialog-subagent.tsx::DialogSubagent`,
  `dialog-select.tsx::{DialogSelect,DialogSelectProps}`,
  `context/event.ts::useEvent.subscribe`, `context/sdk.tsx::useSDK`, and
  `config/keybind.ts`.
- Pane command/default pairs are exactly:

  | Command | Default |
  | --- | --- |
  | `pane.roster` | `<leader>o` |
  | `pane.open.tab` | `<leader>T` |
  | `pane.open.vertical` | `<leader>V` |
  | `pane.open.horizontal` | `<leader>S` |
  | `pane.close` | `<leader>w` |
  | `pane.cycle` | `<leader>.` |
  | `pane.focus.main` | `<leader>0` |

- Compat invalidators are exactly `session.created`, `session.updated`, and
  `session.deleted`. Native invalidators are exactly
  `hya.envelope.properties.event.type` values `member_spawned`,
  `member_status_changed`, `member_finished`, `agent_registered`, and
  `agent_activity_changed`.
- `MemberRunStatus` values are `spawning`, `running`, `done`, `failed`, and
  `cancelled`; terminal values are `done`, `failed`, and `cancelled`.
  `RosterStatus` values are `idle`, `busy`, `done`, and `failed`; terminal values
  are `done` and `failed`. Compat `session.status=idle` never closes a pane.
- Compat invalidation reads `{ type, properties }` and uses only the exact
  top-level `type`. Native invalidation requires this shape:

  ```json
  { "type": "hya.envelope", "properties": { "event": { "type": "member_spawned", "session": "ses_..." } } }
  ```

  `member_status_changed` adds `{ member, status }`; `member_finished` adds
  `{ member, status, child? }`; `agent_registered` adds
  `{ agent_session, handle, agent_type, mode }`; `agent_activity_changed` adds
  `{ handle, status, current_task? }`. Missing/wrong fields are ignored.
- Tree loading has `loading`, `ready`, and `error` states. One request may be in
  flight and one trailing request may be queued. Failed/invalid refreshes retain
  the last valid tree and stop until manager open, explicit retry, or a later
  invalidating event. A generation mismatch discards the response.
- A known child route has no Prompt while loading. Success canonicalizes to
  `tree.session`; failure leaves a read-only transcript plus retry. A successful
  refresh prunes missing observation leaves and focuses Main if needed.
- Reducer transitions are fixed:

  | Action | Result |
  | --- | --- |
  | `openTab(sessionID)` | Focus existing leaf or append one auxiliary tab. |
  | `openSplit(axis, sessionID)` | Focus existing leaf or split the focused leaf and focus the new observation. |
  | `focus(target)` | Remove a focused `closeOnBlur` leaf first, then focus target. |
  | `close(target)` | Main no-op; otherwise remove leaf and collapse empty parent/tab. |
  | `terminal(sessionIDs)` | Remove unfocused matches; mark focused match `closeOnBlur`. |
  | `reconcile(validSessionIDs)` | Remove missing observations and focus Main if focused leaf vanished. |

Each labeled `RED` or `GREEN` item below is one executable step: make one scoped
patch, run its one stated command, and record the result before proceeding. A
RED that passes unexpectedly must be strengthened before product code changes.
A failure unrelated to the named assertion blocks the next step.

## 1. Planning And Baseline Gate

1. Record `git status --short` and exact diffs for every path this plan names,
   especially dirty `subagent-footer.tsx`, `pty-smoke.test.ts`,
   `real-backend.test.ts`, `Cargo.toml`, and `CHANGELOG.md`.
2. Run artifact validation and this isolated plan review. Resolve every `FAIL`.
3. Obtain user approval, then activate only with:

```sh
python3 ./.trellis/scripts/task.py start 07-15-ts-tui-subagent-view
```

Abort before product edits if the review is not `PASS`, task activation fails,
or an overlapping dirty hunk cannot be preserved.

## 2. Backend Contract Slices

### B1 RED - Roster-Aware Assembler

Add `projection_tree::tests::attaches_roster_metadata_by_session` in
`crates/hya-proto/src/projection_tree.rs`. Build root, child, and member-only
nodes plus one `RosterEntry`; assert only the child receives the exact handle,
agent type, status, and current task. It must fail because `RunTreeNode` and
`build_run_tree` do not accept roster data. Serialize all three nodes with
`serde_json::to_value`; assert child JSON contains `roster`, while root and
member-only JSON omit the key entirely (not `null`).

```sh
cargo test -p hya-proto projection_tree::tests::attaches_roster_metadata_by_session
```

### B1 GREEN - Roster-Aware Assembler

Add the optional field, pass a roster-by-session map through `build_node`, and
update all existing `build_run_tree` callers to pass an explicit map (empty
where roster data is unavailable). Do not add a compatibility wrapper. The B1
command must pass, followed by `cargo test -p hya-proto projection_tree`.

### B2A CHARACTERIZATION - Real Nested Spawn

Add `crates/hya-app/tests/nested_spawn_tree.rs` and test
`nested_spawn_reaches_root_tree`. Add only existing workspace test dependencies
`tower` and `http-body-util` under `crates/hya-app/Cargo.toml` if the router
oneshot needs them.

The fixture must construct `SpawnerPlane::new`, attach it to a real
`SessionEngine`, start `ResidentSupervisor` and `spawn_team_supervisor`, create a
root, call `spawner.for_session(root).spawn_background`, then call
`spawner.for_session(child).spawn_background`. Bound all waits with
`tokio::time::timeout`. Request `/session/:child/tree` through
`hya_server::router` and assert `root -> child -> grandchild`. This is expected
to pass as characterization. If it fails, record the evidence, undo only this
characterization test/dependency hunk if it cannot compile independently, return
the task to planning, and obtain a revised approved plan. Do not change
`SpawnerPlane`, `spawn_team_supervisor`, `run_team`, or governor code under this
plan.

```sh
cargo test -p hya-app --test nested_spawn_tree nested_spawn_reaches_root_tree
```

### B2B CHARACTERIZATION - Two-Generation Root-Roster Provenance

In the same real-spawn fixture add
`nested_spawn_registers_two_generations_in_root_roster`. After the bounded
root -> child -> grandchild spawn completes, read the root projection and assert
that `Projection.team.roster` contains entries whose `session` values exactly
match both the child and grandchild. Assert each has a non-empty handle and the
expected agent type and mode; do not assert timing-sensitive status/current-task
values. This is expected to pass because `run_member` records every descendant's
`AgentRegistered` event in its top ancestor log.

If it fails, record the projections/events, stop before B3, and return the task
to planning. Do not compensate in the endpoint, frontend, spawn path, or reducer.

```sh
cargo test -p hya-app --test nested_spawn_tree nested_spawn_registers_two_generations_in_root_roster
```

### B3 RED - Endpoint Roster Projection

In the same fixture add
`tree_endpoint_attaches_roster_to_child_and_grandchild`. After B2B passes,
request `/session/:child/tree`, locate both descendant nodes, and compare each
serialized `roster` object with its exact entry from the root projection. Also
assert root and member-only nodes omit the `roster` key. It must fail because
the endpoint currently discards `Projection.team`.

```sh
cargo test -p hya-app --test nested_spawn_tree tree_endpoint_attaches_roster_to_child_and_grandchild
```

### B3 GREEN - Endpoint Roster Projection

Change only `session_legacy_basic.rs::tree`: preserve the root projection's
roster, index entries by `session`, and pass the index to `build_run_tree`.
Rerun the B3 command, then:

```sh
cargo test -p hya-server --test compat_session_api compat_session_tree_returns_root_node
cargo test -p hya-app --test nested_spawn_tree
```

Backend rollback point: remove only the optional field/index plumbing and the
new tests. No event or stored schema has changed.

## 3. Pure TypeScript Slices

All RED tests go in
`packages/hya-tui-ts/test/subagent-workspace.test.ts`; all GREEN code goes in
the single new `subagent-workspace.ts` module. For every item use:

```sh
cd packages/hya-tui-ts
bun test test/subagent-workspace.test.ts
```

### T1 RED - Wire Validation

Add `validates recursive run tree payloads`; accept nested optional fields and
reject a missing/invalid session-member shape with a typed parse error.

### T1 GREEN - Wire Validation

Add `RunTreeNode`, `RosterEntry`, and `parseRunTree`; parse recursively once at
the HTTP boundary without casts in rendering code.

### T2 RED - Manager Rows

Add `flattens nested rows and marks non-session nodes unselectable`; assert
depth/order, handle/type/status/task search text, root disabled, member-only
disabled, and child selectable.

### T2 GREEN - Manager Rows

Add `flattenRunTree` and `treeSessionIDs`; do not maintain a second hierarchy
index.

### T3 RED - Main State

Add `keeps one uncloseable main leaf`; assert one Main leaf and close-main no-op.
Main-session reset is provided by the existing route-keyed `Session` remount and
is covered at the UI boundary in U4B, not duplicated in reducer state.

### T3 GREEN - Main State

Add `WorkspaceState`, `WorkspaceAction`, `createWorkspaceState`, and
`reduceWorkspace` with only the state needed by the acceptance contract.

### T4 RED - Unique Observation

Add `opens and focuses one observation per session`; open one tab, then open the
same session again and assert one leaf with focus moved in place.

### T4 GREEN - Unique Observation

Implement tab open plus a small recursive leaf scan; add no parallel index.

### T5 RED - Recursive Split

Add `splits only the focused leaf`; perform vertical then horizontal opens and
assert existing leaf identity/order plus the new focused leaf.

### T5 GREEN - Recursive Split

Replace the focused leaf with a fixed equal split; no ratio or minimum terminal
dimension enters state.

### T6 RED - Close And Collapse

Add `closes one leaf and collapses only its parent`; cover split collapse, empty
auxiliary-tab removal, and Main fallback.

### T6 GREEN - Close And Collapse

Implement recursive close/collapse without rearranging siblings.

### T7 RED - Deterministic Focus

Add `cycles focus in tab and visual leaf order`; assert
`Main -> tab order -> depth-first leaves -> Main` and explicit focus-main.

### T7 GREEN - Deterministic Focus

Derive focus order from current state on demand.

### T8 RED - Terminal Cleanup

Add `defers focused terminal closure until focus leaves`; assert immediate
removal for an unfocused terminal leaf and `closeOnBlur` then removal for the
focused leaf.

### T8 GREEN - Terminal Cleanup

Implement terminal reconciliation in the reducer; use no timers.

### T9 RED - Deleted Session Reconciliation

Add `prunes sessions missing from a successful tree`; assert stale leaves are
removed, splits/tabs collapse, and removed focus returns to Main.

### T9 GREEN - Deleted Session Reconciliation

Add one `reconcileSessions` action driven only by a newly validated successful
tree.

### T10 RED - Failed Refresh

Add `retains the last valid tree after a failed refresh`; assert initial failure
gives `error`, later success gives `ready`, and a subsequent HTTP or parse
failure retains that tree with retry available and no automatic call.

### T10 GREEN - Failed Refresh

Add `createRunTreeLoader` using supplied fetch/apply/error callbacks.

### T11 RED - Refresh Coalescing

Add `allows one in flight and one trailing refresh`; control promises and assert
a burst produces exactly two requests, never concurrent requests.

### T11 GREEN - Refresh Coalescing

Add one queued boolean to the loader; no debounce or poller.

### T12 RED - Session Generation

Add `ignores stale generation responses`; change main session before the old
promise resolves and assert no tree/error/layout mutation from that response.

### T12 GREEN - Session Generation

Compare the captured generation/session before applying a result.

### T13 RED - Exact Event Effects

Add `recognizes only tree and terminal event variants`; feed all fixed Compat
and native values. Assert only the eight invalidators refresh; terminal child
IDs are extracted only for member `done|failed|cancelled` and roster
`done|failed`; `session.status=idle`, roster `idle|busy`, malformed envelopes,
and unrelated native events do nothing.

### T13 GREEN - Exact Event Effects

Add narrow raw-event guards plus terminal-session lookup by child ID, member ID,
or roster handle against the validated tree. Do not widen SDK event typing
globally.

Pure-module rollback point: delete the new module/test before UI wiring; no
runtime behavior is connected yet.

## 4. Vertical UI Slices

Before each PTY RED and after each GREEN, build the binaries and run the named
test with this exact command:

```sh
cargo build -p hya-backend -p hya-ts --bins
cd packages/hya-tui-ts
bun test test/pty-smoke.test.ts
```

The PTY proxy may return a deterministic `/session/:root/tree` fixture while all
session/message requests continue to use the real backend. The Rust B2A/B2B/B3 tests
already prove the real spawn/projection/endpoint path, so the PTY must not add a
test-only product endpoint.

### U1A RED - Pane Keybind Contract

Add `exposes exact pane command defaults` to
`test/subagent-workspace.test.ts`. Import `Definitions` and `CommandMap` from
`config/keybind.ts`; assert all seven command/default rows in Fixed Contracts and
assert the mappings are distinct. It must fail because those definitions do not
exist.

```sh
cd packages/hya-tui-ts
bun test test/subagent-workspace.test.ts
```

### U1A GREEN - Pane Keybind Contract

Add only the seven definitions and `CommandMap` entries to `config/keybind.ts`.
Keep the four legacy definitions until U7. Rerun the U1A command and
`bun run typecheck`.

### U1B RED - Session Tree Resource

Extend the existing child-observation PTY proxy with two child sessions, one
grandchild, unique transcripts, and a deterministic `/session/:root/tree`
response. Assert entering the root Session route performs exactly one tree GET.
Current code must fail with zero tree GETs.

### U1B GREEN - Session Tree Resource

In `index.tsx::Session` only, instantiate the T10-T12 loader using `sdk.fetch`
and `sdk.url`, fetch on route entry, subscribe through `useEvent.subscribe` using
the T13 guard, reconcile successful trees, and clean up the subscription with
the Session component. Do not register commands or render the manager yet. The
U1B PTY, pure tests, and `bun run typecheck` must pass.

### U1C RED - Recursive Manager

Press `Ctrl+X`, then `o`; assert the manager shows root, both children, pending
member-only row, and grandchild in depth-first order with handle/type/status/task
labels. Enter `/` plus the grandchild handle and assert only that matching branch
remains; first Escape exits filter mode, second Escape closes without changing
layout. Current code must fail because `pane.roster` is not registered.

### U1C GREEN - Recursive Manager

Add `DialogSelectProps.retainDisabled` and
`filterActivation: "immediate" | "slash"` with unchanged defaults. Rewrite
`DialogSubagent` to render flattened resource rows, disabled root/pending rows,
loading/error/retry states, and open/focused markers. Register only
`pane.roster` in `Session` and open that dialog with tab placement. The U1C PTY,
pure tests, and `bun run typecheck` must pass.

### U1D RED - Direct Placement Commands

Independently press `<leader>T`, `<leader>V`, and `<leader>S`; assert each opens
the same manager with tab, vertical, or horizontal placement selected and that
cancel leaves layout unchanged. Current code must fail because the commands are
not registered.

### U1D GREEN - Direct Placement Commands

Register only `pane.open.tab`, `pane.open.vertical`, and
`pane.open.horizontal` in `Session`; each passes its fixed placement to the same
`DialogSubagent`, while `v` or `s` overrides it inside the dialog. The U1D PTY
and `bun run typecheck` must pass.

### U2A RED - Unique Observation Hydration

Add `Linux PTY hydrates each observation once`. Select one child through the
manager and record the existing child-session GETs made by
`sync.session.sync` (`session`, `message`, `todo`, and `diff`). Select/focus the
same child again and assert each child hydration endpoint was requested exactly
once while the visible route remains the root. Current code must fail because
workspace leaves do not trigger child synchronization.

### U2A GREEN - Unique Observation Hydration

In `index.tsx::Session`, react only to the unique observation session IDs already
present in reducer state and call the existing idempotent
`sync.session.sync(sessionID)` as each ID first appears. Add no renderer, cache,
event subscription, or pane-specific store. The U2A PTY, pure tests, and
`bun run typecheck` must pass.

### U2B RED - Reused Observation Transcript

Add `Linux PTY renders one observation with the shared transcript`. Select one
child and assert its read-only header and unique stored transcript marker appear,
while the child pane does not render the root marker as its own content. Do not
assert Prompt, focus return, or answer controls in this slice.

### U2B GREEN - Reused Observation Transcript

Extract the existing message loop in `index.tsx::Session` into a local
session-parameterized pane renderer using the same `UserMessage` and
`AssistantMessage` components. Render one observation tab with its own scroll
ref and compact read-only header. Do not change Prompt placement, answer
controls, child-route behavior, or command registration. The U2B PTY, pure
tests, and `bun run typecheck` must pass.

### U2C RED - Main Prompt And Input Isolation

Migrate the nonce-draft portion of the existing child-observation PTY test.
Focus the observation, assert Prompt text is absent, type a unique sentinel plus
Enter, and assert no root/child prompt request or event occurs. Press Escape to
focus Main and assert the original draft is unchanged.

### U2C GREEN - Main Prompt And Input Isolation

Keep exactly one Prompt mounted inside Main, target it only at `mainSessionID`,
and hide/blur it whenever an observation owns focus. Consume ordinary printable
input and Enter in observation focus while retaining navigation, scrolling,
copy, Escape, and manager handling. Do not touch permission/question selection
or child-route recovery. The U2C PTY, pure tests, and `bun run typecheck` must
pass.

### U2D RED - Main-Owned Answer Controls

Add `Linux PTY keeps descendant answers in Main`. Reuse the real permission and
question lifecycle setup already exercised by `test/real-backend.test.ts`, one
request at a time, but originate each request from the grandchild. Assert the
active observation never renders an answer control or sends a reply; after
focusing Main, each control appears exactly once and its reply targets the
originating request. Current immediate-child aggregation must omit the
grandchild and fail.

### U2D GREEN - Main-Owned Answer Controls

Replace the immediate-child request scan with the validated
`treeSessionIDs(tree)` set and render the existing `PermissionPrompt` and
`QuestionPrompt` only inside Main. Do not add an observation control or another
request projection. The U2D PTY, pure tests, and `bun run typecheck` must pass.

### U2E RED - Child Route Canonicalization

Start the PTY on a known child route with a deliberately delayed valid tree
response. Assert no Prompt or submission is possible while loading, then assert
the validated `tree.session` becomes the sole Main route and workspace root.
Do not exercise failure handling in this slice.

### U2E GREEN - Child Route Canonicalization

When the current route is a known child, structurally omit Prompt until the tree
loader returns a validated root. Navigate only to that `tree.session`; reuse the
existing loader generation guard. Add no failure UI, retry, or fallback viewer.
The U2E PTY, pure tests, and `bun run typecheck` must pass.

### U2F RED - Child Route Read-Only Recovery

Start on a known child and make the first tree response fail HTTP or validation.
Assert the child transcript remains visible and read-only, retry is visible,
Prompt/submission remain absent, and no automatic second GET occurs. Press `r`
and assert exactly one new request; a valid response then canonicalizes to root.

### U2F GREEN - Child Route Read-Only Recovery

Wire only the existing loader's error and explicit-retry states into the child
recovery view. Preserve the child transcript, stop after failure, and reuse U2E
on a later valid result. Add no retry loop or compatibility viewer. The U2F PTY,
pure tests, and `bun run typecheck` must pass.

### U3 RED - Repeated Splits Stay Live

In the PTY, reopen the manager and use `v` then `s` to place two different
sessions. Assert two unique initial transcript markers are simultaneously
present, the same session cannot duplicate, and focus changes erase neither
pane. Then deliver one distinct later message update to each observed session
through the existing backend/SSE path and assert both late markers are visible
simultaneously without another tree GET or changed pane identity. Restrict the
late-marker assertion to output captured after a deterministic redraw so the
cumulative PTY transcript cannot false-pass.

### U3 GREEN - Recursive Live Split Rendering

Recursively render reducer split nodes as native equal-flex row/column boxes with
`minWidth={0}`, `minHeight={0}`, independent scroll refs, and width-safe headers.
Each leaf reads the existing reactive sync store by its own session ID; do not
capture transcript snapshots or add another subscription. Do not add dimensions
to reducer state. The U3 PTY and pure tests must pass.

### U4A RED - Tabs, Focus, And Close

Use `<leader>T` to create a second auxiliary tab, `<leader>.` to cycle,
`<leader>w` to close one leaf, and `<leader>0` to return to Main. Assert hidden
tabs preserve their transcript/scroll marker, close collapses only its owning
layout, Main cannot close, and the root draft remains unchanged.

### U4A GREEN - Mounted Tabs And Commands

Render all tabs mounted and hide inactive tabs. Route cycle, close, and focus-main
commands only through `reduceWorkspace`, then register `pane.close`,
`pane.cycle`, and `pane.focus.main`; no pane command may navigate to a child
route. The U4A PTY and pure tests must pass.

### U4B CHARACTERIZATION - Main Route Resets Workspace

Under root A, create an auxiliary tab and repeated split, then use the existing
`session.list` command with `<leader>l` to navigate to root B. Assert every A
observation/open marker is gone, cycling cannot reveal a hidden A tab, B has only its fresh Main
workspace, and submission targets B. This is expected to pass because
`app.tsx::App` renders `Session` inside a keyed `Show` keyed by
`route.data.sessionID`; no reducer reset action or effect is planned. If it
fails, preserve the failing test, stop, and return to planning before U5.

Run the standard PTY command, pure tests, and `bun run typecheck`.

### U5 RED - Narrow And Wide Layout

Parameterize the PTY helper to run the split scenario at 80 and 140 columns.
Assert both transcript markers and distinct read-only headers remain present,
text does not concatenate across pane boundaries, and resize/re-render does not
change pane count or focus order.

### U5 GREEN - Width-Safe Rendering

Apply only native flex constraints and existing truncation helpers to headers,
tab labels, and transcripts. The U5 PTY command must pass at both widths.

### U6 RED - Task Workspace Entry

Make the PTY root message fixture include a task part whose `metadata.sessionId`
matches an existing tree node. Assert its hint uses the configured
`pane.roster` shortcut and contains no `view subagents` text. Current code must
fail with the legacy hint.

### U6 GREEN - Task Workspace Entry

Add the workspace open/focus callback to the existing Session render context;
`index.tsx::Task` invokes it instead of `navigate` and shows the configured
`pane.roster` shortcut. Preserve retry-error display. The U6 PTY, pure test, and
typecheck commands must pass.

### U7 RED - Legacy Route Reachability

Start from Main with a nonce draft and record proxy requests. Send the four old
paths: `Ctrl+X` then `Down`, `Right`, `Left`, and `Up`. Assert the visible/root
session and draft never change and no GET targets a child Session route. Current
code must fail because the legacy child command remains registered. Also add the
supplemental source absence gate; it must fail before deletion.

```sh
if rg -n --glob '*.{ts,tsx}' \
  'session\.child\.(first|next|previous)|session\.parent|SubagentFooter|view subagents' \
  packages/hya-tui-ts/src; then exit 1; fi
```

### U7 GREEN - Legacy Deletion

- Delete `subagent-footer.tsx`, its import/render, `enterChild`,
  `moveFirstChild`, `moveChild`, `childSessionHandler`, the four legacy command
  entries, and the four legacy keybind definitions/defaults.
- Preserve all unrelated dirty PTY and backend-test hunks.

The U7 PTY, pure test, typecheck, and source absence gate must pass. Only now is
the old viewer removed.

## 5. Complete Verification

From `packages/hya-tui-ts`:

```sh
bun run typecheck
bun test
bun run build
```

From repository root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p hya-backend -p hya-ts --bins
```

Then rerun the focused executable contracts:

```sh
cargo test -p hya-app --test nested_spawn_tree
cd packages/hya-tui-ts
bun test test/subagent-workspace.test.ts test/pty-smoke.test.ts
```

Run `git diff --check`, inspect `git diff` and `git status --short`, run the
Trellis full-scope check, and validate the task:

```sh
python3 ./.trellis/scripts/task.py validate .trellis/tasks/07-15-ts-tui-subagent-view
```

Acceptance mapping:

| Contract | Evidence |
| --- | --- |
| `AC-02` to `AC-04`, `AC-16` | B1, B2A, B2B, and B3 Rust tests |
| `AC-05` to `AC-08`, `AC-10` to `AC-12`, `AC-17` | T3-T13 pure tests |
| `AC-01`, `AC-05` to `AC-15`, `AC-17` | U1A-U4B and U5-U7 PTY, keybind, reachability, and source gates |
| `AC-18` | Complete Bun/Rust/build/Trellis gate |

Any failed final gate blocks release edits, commit, push, and task completion.

## 6. Mandatory Release And Delivery Gate

Repository `AGENTS.md` imposes all three requirements: the **Commit Rule** and
**Feature Workflow Rule** require one verified atomic feature commit and push;
the **Release & Changelog Rule** requires every feature to update
`[workspace.package].version`, keep the TS package/release metadata aligned, and
keep only the newest release notes in root `CHANGELOG.md` while archiving the
previous version under `docs/changes/`.

The current dirty `Cargo.toml` and `CHANGELOG.md` already claim `0.33.3`, while
`packages/hya-tui-ts/package.json` claims `0.33.2`. Before any metadata patch,
ask the user to identify whether `0.33.3` is an independently landing baseline
or the release this feature joins:

- If it lands first, require that clean baseline, then bump this feature to the
  next version and archive the prior root changelog.
- If this feature joins it, preserve the existing notes/hunks and add only this
  feature's aligned TS version and newest release notes.
- If ownership remains unclear, stop with verified product changes uncommitted;
  do not stage, commit, push, or report the feature complete.

After the authorized metadata change, rerun the complete verification gate.
Stage only task-owned paths. Commit and push only after explicit user delivery
authorization, then finish/archive the Trellis task and record the handoff.

## Rollback Sequence

1. Before U6, remove only new Session/manager wiring and the pure module/test;
   the legacy viewer is still present.
2. Revert only task-owned backend roster/index hunks and tests; no data migration
   or event rollback exists.
3. After U6, restore the captured task-owned legacy footer/keybind/route hunks
   before removing the replacement, then rerun the original read-only PTY check.
4. Revert only task-owned release hunks if metadata verification fails.

Never use worktree-wide reset/checkout. Leave every unrelated baseline change
untouched.
