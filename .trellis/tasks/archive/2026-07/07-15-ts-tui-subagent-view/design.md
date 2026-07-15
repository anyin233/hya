# Design

## Architecture

The feature remains one Session-screen workspace. The backend owns recursive
lineage and roster state; the TypeScript TUI owns only ephemeral presentation
state.

```text
event log
  -> Projection / TeamProjection
  -> GET /session/:id/tree
  -> validated RunTreeNode resource
  -> pure workspace reducer
  -> existing transcript renderer in tabs and split panes
```

Stored messages remain the only transcript/copy/export source. Member and team
lifecycle events update the tree and headers; they never become synthetic
messages.

## Backend Contract

`RunTreeNode` in `crates/hya-proto/src/projection_tree.rs` remains the recursive
tree contract. Add one optional `roster: RosterEntry` field so each session node
can expose the authoritative handle, agent type, roster status, and current task
required by the manager. Change `build_run_tree` to accept a roster index keyed
by `SessionId`; every current caller must pass that index explicitly. The root
and member-only pending nodes omit `roster`.

The tree endpoint already reads each full `Projection` before retaining its
`SessionProjection`. `run_member` resolves every spawned session's top ancestor
and writes `AgentRegistered` to that root log, so child and grandchild metadata
share the root `TeamProjection.roster`. A characterization test must prove both
generations before the endpoint retains that root roster, indexes entries by
`RosterEntry.session`, and lets the shared tree assembler attach the matching
optional entry. No new endpoint or frontend team reducer is added.

`member.status` remains authoritative for transient-member cleanup. Its exact
wire values are `spawning`, `running`, `done`, `failed`, and `cancelled`; only
the last three are terminal. Roster status is `idle`, `busy`, `done`, or
`failed`; only `done` and `failed` are terminal. Roster status is used for the
live header/manager label when present; `member` metadata and the existing
Compat session status are display fallbacks. Compat `session.status=idle` never
closes a pane. An absent roster entry does not hide a valid tree node.

The existing child-scoped `ToolCtx.spawner.for_session(session)` and governor
remain unchanged. A cross-layer test must drive a child-session task spawn and
assert that `/session/:root/tree` returns the resulting grandchild.

## TypeScript Boundary

Add `routes/session/subagent-workspace.ts` as the only new product module. It
owns:

- the runtime validator and TypeScript shape for `RunTreeNode`;
- tree flattening/search text and terminal-session extraction;
- the small pure reducer for tabs, split trees, focus, close, reset, and
  terminal reconciliation;
- the narrow type guard that identifies tree-invalidating Compat/native events.

HTTP JSON is validated once at this boundary. Rendering code consumes the
validated type and does not cast raw payload fields.

`Session` owns the resource lifecycle and calls `sdk.fetch` against
`${sdk.url}/session/:id/tree`. It fetches on initial entry, manager open, and an
explicit manager retry. It refreshes for Compat `session.created`,
`session.updated`, and `session.deleted`, plus `hya.envelope` values whose
`properties.event.type` is exactly `member_spawned`, `member_status_changed`,
`member_finished`, `agent_registered`, or `agent_activity_changed`. Other
events, including `session.status`, do not invalidate the tree.

The request coordinator permits one in-flight request and one trailing refresh.
A main-session generation check rejects old responses. A malformed response is
the same as a failed request: retain the last valid tree, set an error for the
manager, and do not retry automatically. Reopening the manager or pressing its
retry action starts one new request; this is the stop condition, so there is no
poller or retry loop.

The existing SDK event batching and raw `fetch`/base URL are reused. There is no
poller and no change to the general Sync projection.

Because the tree endpoint resolves the top ancestor, entering the Session route
with a child session canonicalizes the route to the returned root. While a
known child route is loading, the Prompt is structurally omitted. On success,
`tree.session` must be present and the route navigates to that root. On HTTP or
validation failure, the child transcript remains visible in an explicit
read-only recovery state with retry available; it never gains a Prompt. This
removes interactive whole-route child sessions while keeping failed deep links
safe.

## Workspace State

```text
WorkspaceState
  mainSessionID
  tabs[]
  activeTabID
  focused leaf

Tab
  id
  root: Main | Observation(sessionID, closeOnBlur) | Split(axis, first, second)
```

The first tab contains the unique `Main` leaf. Auxiliary tabs contain
observation leaves. A split replaces the focused leaf with a fixed 1:1 split
whose first child is the existing leaf and whose second child is the newly
focused observation.

Reducer rules:

- `open(tab)` appends and activates an auxiliary tab.
- `open(vertical|horizontal)` splits the focused leaf right/below.
- Opening an existing session scans the small layout trees and focuses it in
  place; it never creates a duplicate or silently rearranges an existing
  layout.
- Closing `Main` is a no-op. Closing another leaf collapses its parent split;
  removing the last leaf removes that auxiliary tab.
- Focus cycles from `Main` through tabs in tab order and leaves in visual
  depth-first order, then wraps to `Main`.
- `Escape` and `pane.focus.main` activate and focus the main tab.
- A root-route change remounts `Session` through the existing keyed `Show` in
  `app.tsx::App`, which recreates one fresh Main tab without a duplicate reducer
  reset path. A PTY characterization test locks down that wiring.
- A successful tree refresh removes observation leaves whose session IDs no
  longer exist, collapses their owning splits/tabs, and focuses `Main` if the
  focused leaf was removed. The existing app-level root-session deletion path
  still navigates home.
- Terminal reconciliation removes every unfocused terminal leaf immediately.
  A focused terminal leaf is marked `closeOnBlur` and is removed before the
  next focus transition completes.

The reducer derives uniqueness by scanning leaves. Expected layouts are small,
so a second index and synchronization logic are unnecessary.

## Rendering And Input

`index.tsx` keeps command registration and the existing message components. Its
current transcript block becomes a local reusable pane renderer parameterized
by session ID, main/observation mode, focus, and scroll ref. This avoids a
second transcript implementation and keeps copy/export semantics unchanged.

Tabs stay mounted and are hidden when inactive so Prompt draft/cursor and pane
scroll state survive tab changes. Split nodes recursively render native OpenTUI
row/column flex boxes with equal children, `minWidth={0}`, `minHeight={0}`, and
width-safe truncation. Terminal shrink clips/truncates content but never mutates
the layout. No guessed split-size threshold or ratio editor is introduced.

Observation leaves contain:

- a compact tonal header with handle, agent type/status/current task,
  placement, open/focused state, and `Read-only`;
- the existing stored-message transcript renderer and independent scroll box;
- no Prompt, permission prompt, question prompt, or route-changing action.

The one main Prompt remains mounted and targets only `mainSessionID`. It is
visible only while `Main` is focused and no global permission/question overlay
owns input. Ordinary text and Enter are intercepted while an observation leaf
is focused; observation navigation, scrolling, copy selection, close, cycle,
and manager commands remain active.

Permission and question requests are collected for all session IDs in the
validated tree and rendered only in the main leaf. Observation headers may show
attention state but never answer controls.

`Task` transcript rows call a workspace callback rather than navigating to a
child route. A known child opens or focuses a read-only tab, and the task hint
uses the configured `pane.roster` shortcut.

## Manager And Commands

Rewrite the existing `dialog-subagent.tsx` as the manager. It consumes the
flattened recursive tree, renders indentation and live metadata, and marks
open/focused rows. Root and member-only rows remain visible but cannot commit;
cancel, empty results, and invalid rows leave layout unchanged.

Reuse `DialogSelect` by adding two optional `DialogSelectProps` capabilities:

- `retainDisabled`, default `false`, keeps disabled rows visible while keyboard
  and mouse commit skip them;
- `filterActivation: "immediate" | "slash"`, default `"immediate"`, starts
  filtering only after `/` in slash mode, with Escape leaving filter mode before
  the dialog closes.

Existing callers keep their current defaults. Manager actions map `Enter`, `v`,
and `s` to tab, vertical, and horizontal placement. The Session command set is:

| Command | Default |
| --- | --- |
| `pane.roster` | `<leader>o` |
| `pane.open.tab` | `<leader>T` |
| `pane.open.vertical` | `<leader>V` |
| `pane.open.horizontal` | `<leader>S` |
| `pane.close` | `<leader>w` |
| `pane.cycle` | `<leader>.` |
| `pane.focus.main` | `<leader>0` |

Direct placement commands open the same manager with placement preselected;
`Enter` commits that placement, while `v` or `s` may override it.

The manager has three resource states. `loading` shows a visible disabled
"Loading subagents" row. `error` shows a visible disabled "Subagent tree
unavailable" row and an `r` retry action. `ready` shows the flattened tree.
Selecting a session that disappeared since the last successful refresh is a
no-op, reports the stale selection, and triggers one refresh.

## Implementation References

- `packages/hya-tui-ts/src/upstream/routes/session/index.tsx::Session` owns the
  route, command registration, tree resource, pane rendering, Prompt, global
  permission/question controls, and task-row workspace callback.
- `index.tsx::Task` currently navigates to child routes and must call that
  workspace callback instead.
- `packages/hya-tui-ts/src/upstream/routes/session/dialog-subagent.tsx::DialogSubagent`
  becomes the recursive manager.
- `packages/hya-tui-ts/src/upstream/ui/dialog-select.tsx::DialogSelect` and
  `DialogSelectProps` receive only the two opt-in behaviors above.
- `packages/hya-tui-ts/src/upstream/context/event.ts::useEvent.subscribe` is the
  event entry point; `packages/hya-tui-ts/src/upstream/context/sdk.tsx::useSDK`
  supplies `fetch` and `url`.
- `crates/hya-server/src/compat/session_legacy_basic.rs::tree` reads the full
  root `Projection`, indexes `Projection.team.roster`, and calls the shared
  assembler.
- `crates/hya-app/src/runtime.rs::spawn_team_supervisor` plus
  `SpawnerPlane::for_session` is the executable nested-spawn seam. No test or
  product code may introduce a second spawn path.

## Compatibility And Deletion

The replacement removes:

- `session.child.first`, `session.child.next`, `session.child.previous`, and
  `session.parent` commands and bindings;
- route-navigation helpers for parent/sibling child sessions;
- `SubagentFooter` and `subagent-footer.tsx`;
- the old task-row child navigation and `<leader>down` hint;
- the old one-action `DialogSubagent` behavior.

There is no compatibility alias or second viewer. The HTTP tree field is
additive and optional; all layout state is ephemeral, so rollback requires no
data migration.

## Planner Merge

All four planners agreed on a local pure reducer, recursive server-owned tree,
existing transcript reuse, event-driven refetch, full legacy deletion, and no
new dependency or spawn subsystem. The main-agent merge resolved these
differences:

| Disagreement | Resolution |
| --- | --- |
| Derive roster labels in TS vs extend the tree DTO | Extend `RunTreeNode` with optional `RosterEntry`; no existing HTTP route exposes authoritative handle/current-task data. |
| Relocate an already-open pane vs focus it in place | Focus in place. The task requires deduplication/focus, and relocation would unexpectedly rewrite a recursive layout. |
| Add fixed split admission thresholds vs rely on native flex | Use native equal flex, truncation, and narrow PTY coverage. No product requirement supplies arbitrary minimum dimensions. |
| Build a custom selector vs extend `DialogSelect` | Add only opt-in visible-disabled and slash-filter behavior to the existing selector. |
| Keep child deep links as read-only routes vs canonicalize | Canonicalize to the tree root so every route has one main input owner and no legacy viewer survives. |
| Put the nested contract in dirty TS backend tests vs Rust server coverage | Use the existing Rust server/fake-provider seam; leave unrelated `real-backend.test.ts` edits untouched. |

The round-3 correction ran all four planners again. The merge kept the common
minimum: characterize child and grandchild root-roster entries before endpoint
work; separate hydration, rendering, input, answer controls, canonicalization,
and recovery; prove later updates in both split panes; characterize the existing
route-keyed reset; and scan only product source when deleting legacy paths.

## Risks And Recovery

- **Input leakage:** structurally omit aux input and assert zero prompt requests
  in PTY coverage.
- **Unavailable/invalid tree:** root routes remain main-only; known child routes
  remain read-only. Retain the last valid tree and retry only on manager open,
  explicit `r`, or a later invalidating event.
- **Stale tree response:** generation-check requests and discard the stale
  response without changing layout or error state.
- **Deleted observation:** reconcile every successful tree into the workspace;
  collapse missing leaves and return focus to `Main` when needed.
- **Layout corruption:** prove every reducer transition before rendering it.
- **Wrong terminal closure:** close only from explicit terminal member status,
  never Compat `idle`.
- **Narrow overlap:** use native flex constraints and test at 80 columns plus a
  wider terminal.
- **Dirty worktree:** patch/stage exact files and preserve existing test hunks.
  Deleting the superseded footer is intentional.
- **Release metadata:** do not edit or stage the existing uncommitted `0.33.3`
  files until their ownership and target version are resolved.

Rollback is ordered and hunk-scoped:

1. Before the legacy deletion, remove the new Session commands/manager wiring
   and the pure workspace module; the untouched legacy viewer remains usable.
2. Revert only the task-owned `RunTreeNode`, assembler, and tree-handler hunks;
   the optional JSON field has no stored representation or migration.
3. After legacy deletion, restore the captured task-owned footer/keybind/route
   hunks before removing the replacement, then rerun the old PTY assertion.
4. Never use worktree-wide checkout/reset. Preserve every pre-existing dirty
   hunk captured before implementation.

No layout is persisted and no event/store schema changes, so rollback requires
no data or configuration cleanup.
