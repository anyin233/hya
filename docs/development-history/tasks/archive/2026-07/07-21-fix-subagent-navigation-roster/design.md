# Technical Design: Subagent Navigation and Roster Dock

## Status

Proposed for review. No production code changes may begin until this task is
activated.

## Scope

This change repairs ownership lookup for parentless roster members, routes one
bare `Esc` from every subagent observation path to the Team root, and renders one
shared roster/shortcut dock in the existing Rust TUI session and auxiliary-pane
screens.

The change does not alter backend lifecycle events, add shortcuts, replace the
sidebar roster, add runtime state, or move app-specific rendering into
`hya-tui-lib`.

## Root Cause

Team projections are keyed by the root session. A failed-start child can be
present in that Team roster while its cached `Session` has no `parent_id`.
`MessageStore::team_root_for` currently follows only `Session.parent_id`, so it
returns the child itself. This loses the owning Team for status classification,
roster dialogs, split selection, and navigation.

The two `Esc` failures then enter different runtime paths:

- A direct child route is main-focused. Bare `Esc` is not handled by the prompt
  path and reaches the root-session interrupt command.
- A focused auxiliary pane handles bare `Esc` by focusing main, but the main
  route may itself still be the child.

## Contracts

### Team Root Resolution

`MessageStore::team_root_for(session_id)` keeps its current borrowed-string API
and precedence:

1. Follow cached non-empty `Session.parent_id` ancestry. A resolved ancestor is
   authoritative.
2. If the starting session owns a Team projection, keep the starting session as
   the root.
3. Otherwise, inspect Team rosters for entries whose `session` equals the
   starting session. Return a different Team root only when exactly one matches.
4. If there are zero or multiple roster owners, return the starting session.
5. Preserve the existing cycle guard and cycle fallback.

This repairs incomplete failed-start projections without guessing when local
state is ambiguous.

### Subagent Classification

`session::subagent_status` preserves explicit parent metadata as its first
source. When no parent exists but `team_root_for(session_id) != session_id`, it
classifies the route as `Child` from the owning Team roster. The child index and
total come from distinct, non-empty, non-root roster session IDs in stable
order.

Root sessions continue to derive parent counts and attention from the Team
roster or cached child sessions.

### Escape Navigation

The runtime adds one private `return_to_team_root` helper using the same event
loop pattern as `goto_parent`:

1. Capture the current route session, shared store, and event sender.
2. Spawn a short task that reads `team_root_for` under the existing async read
   lock.
3. Send `AppEvent::LoadSession(root)` only when the resolved root differs from
   the current route.

No new event kind or async key-dispatch API is needed. `LoadSession` already
resets prompt state, clears auxiliary panes through normal route navigation,
and starts session backfill. The headless harness already drains spawned events
during one `press` call.

Direct-route behavior:

- In `handle_prompt_key`, unmodified `Esc` on a classified child invokes
  `return_to_team_root` regardless of prompt contents.
- On a root route, bare `Esc` still reaches `session.interrupt`.

Auxiliary behavior:

- In `handle_observation_key`, unmodified `Esc` first focuses main and then
  invokes `return_to_team_root`.
- If the underlying main route is already the Team root, the helper is a no-op,
  so the existing focus-only split behavior is preserved.
- If the underlying main route is a child, normal session loading returns to the
  root and clears the now-stale split.

Permission, question, dialog, status overlay, and pending-leader precedence in
`handle_key` remains unchanged.

### Shared Roster and Shortcut Dock

The app-specific dock lives in `crates/hya-tui/src/screens/session.rs` and is
called by both session and auxiliary-pane renderers. It returns styled
`render::text::Text`; it does not perform terminal I/O or own layout state.

Content order is deterministic:

1. Existing parent/child status summary.
2. Every Team roster entry with a non-empty session different from the Team
   root, including the currently observed child, in `BTreeMap` handle order.
3. Three compact shortcut groups below the roster.

Roster rows reuse projected `handle`, `agent_type`, `mode`, `status`, and
`current_task` values plus existing semantic status colors. Empty optional
fields are omitted.

The shortcut groups select these existing rows from
`keymap::default_binding_specs()` and use their binding text:

- Navigate: `session_background`, `session_child_first`,
  `session_child_cycle`, `session_child_cycle_reverse`, `session_parent`.
- Open: `pane_roster`, `pane_open_tab`, `pane_open_vertical`,
  `pane_open_horizontal`.
- Pane: `pane_channels`, `pane_close`, `pane_cycle`, `pane_focus_main`.

The renderer owns only compact action labels and grouping. It does not duplicate
key strings or add commands. Child and auxiliary surfaces also show contextual
`Esc` as return-to-main; root-session `Esc` remains interrupt and is not relabeled.

The complete text is wrapped with the existing grapheme-aware `Text::wrap`.

### Layout

Main session:

- Compute prompt height first, preserving its current minimum of six rows.
- Build and wrap the dock to `main_area.width`.
- Reserve as many dock rows as fit after the prompt using saturating arithmetic.
- Place the dock immediately before the prompt with the same `x` and `width` as
  `main_area` and the prompt.
- Give remaining rows to the transcript. Sidebar behavior is unchanged.

Auxiliary pane:

- Preserve the one-row read-only header.
- Build and wrap the same dock to the pane width.
- Carve the dock from the bottom of the body with saturating arithmetic.
- Give the remaining body rows to the transcript and update its viewport height
  from that reduced rectangle.
- Do not render a prompt composer.

On terminals too short to display every row, prompt/header and valid geometry
take precedence; normal 80- and 120-column fixtures must show every configured
shortcut without horizontal overflow.

## Data Flow

```text
hya.envelope Team events
  -> MessageStore.teams[root].roster
  -> team_root_for(route session)
  -> subagent_status and shared dock

bare Esc
  -> direct prompt path or auxiliary observation path
  -> return_to_team_root
  -> AppEvent::LoadSession(root)
  -> existing route navigation and redraw
```

## Verification

- `hya-sdk` unit tests cover explicit ancestry, own-Team authority, one unique
  roster owner, ambiguous roster owners, missing sessions, and cycles.
- `hya-tui` unit coverage proves roster-only child classification.
- Harness coverage proves one `Esc` returns from a direct failed-start child.
- Harness coverage proves a sibling split is created from that child and one
  `Esc` then restores the root main view.
- Semantic render tests at 80 and 120 columns prove all selected bindings are
  present, wrapped lines stay within width, the main dock directly precedes the
  composer, and the auxiliary dock occupies the bottom without a composer.
- Existing split placement, root interrupt, and overlay precedence tests remain
  the regression baseline.

## Compatibility and Rollback

The wire protocol, persisted events, public TUI keymap, and public crate APIs do
not change. Ambiguous incomplete projections retain current behavior. The patch
is reversible by restoring the lookup, key branches, and two renderer layouts;
no data migration or compatibility path is required.

The release step must re-read the then-current workspace version and changelog.
They are currently modified by another active task at `0.33.17`; this task must
not overwrite those changes. Once available, this fix takes the next patch
version, archives the prior root changelog, and writes only this release's notes
to root `CHANGELOG.md`.
