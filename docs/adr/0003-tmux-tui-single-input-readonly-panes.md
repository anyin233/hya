# tmux-style TUI: single input, read-only subagent tabs and panes

The multi-agent TUI is a tmux-style layout: an always-present, **uncloseable main-agent window**;
a Prompt composer that appears only with the main view and routes **exclusively to the main agent**;
and user-launchable **read-only** panes/tabs for observing other agents live, plus roster and channel overlays. We
chose a single control channel (user ↔ main agent ↔ Team) over letting the user type directly to any
focused subagent because it matches the actor model — residents wake only via mail (ADR-0002), so
there is exactly one place user intent enters the system. This keeps the Prompt composer and single-control-channel model simple
and makes "which agent am I talking to?" un-ambiguous.

## Status

Revisited for the subagent-manager redesign: this ADR supersedes its earlier tab-only consequence
(focused pane full-frame plus tab bar, with true side-by-side split deferred) while keeping the
original single-input invariant.

## Consequences

- The input invariant is enforced structurally: observation views never receive Prompt composer input,
  and text typed while an observation view is focused is ignored unless it is a defined
  observation/navigation shortcut. This deliberately supersedes the earlier tested behavior where
  typing while a legacy aux pane was focused still edited/submitted the main Session; that harness test
  must be replaced with coverage for ignored text and unchanged main prompt state.
- Subagent observation views may be presented as either focused tabs or side-by-side split panes.
  Both placements preserve the same invariant: only the visible main-agent Prompt composer accepts
  user input. Subagent focus controls observation actions such as scroll, close, cycle, split/tab
  placement, and manager navigation; it never buffers or submits prompt text.
- Escape in an observation view returns focus to the main view and restores the Prompt composer;
  closing an observation view remains an explicit close action.
- Navigation initially reuses existing observation controls: Ctrl+X . cycles focus, Ctrl+X W closes
  the focused observation view, and Escape returns to the main view. No dedicated tab-next/tab-prev
  bindings are introduced for this redesign.
- To redirect a subagent the user tells the *main* agent, which messages/re-tasks it — there is no
  direct-to-subagent input.
- Permission and question prompts raised by subagents remain global main-owned modals. Observation
  views may show blocked status, but they never host answer/input controls.
- Opening a new tab or split is a two-step commit through the same Subagent selector. Ctrl+X O
  opens the Subagent manager; inside it, Enter opens the selected subagent as a tab, `v` opens a
  vertical left/right split, and `s` opens a horizontal top/bottom split. Cancel or an empty live
  Roster leaves the current layout unchanged; an observation view closes only when an explicit
  terminal team lifecycle event marks that observed subagent finished.
- Direct commands open the same selector with placement preselected, then commit only after a live
  subagent row is selected: Ctrl+X Shift+T for tab, Ctrl+X Shift+V for vertical left/right split,
  and Ctrl+X Shift+S for horizontal top/bottom split, preserving existing lowercase leader chords.
  In this preselected mode, Enter commits the preselected placement; explicit placement keys may
  override it before commit.
- Each observation view renders the subagent Session's transcript plus a compact status header
  (handle, agent type/status/current task, placement, read-only marker) and omits the Prompt composer.
- Observation transcript scrolling follows new output until the user manually scrolls; manual scroll
  pins that view and surfaces a new-output indicator until the user returns to bottom.
- A live subagent handle owns at most one observation view. Selecting an already-open subagent focuses
  that view and moves it to the requested placement when the action asked for a different tab/split.
- Manager rows show handle, agent type, status, current task, and an open/focused marker when an
  observation view already exists for that handle.
- The Subagent manager presents the current Team Roster as a tree/indented list when parent-child
  spawn relationships are known. `main` may appear as a non-selectable root row; only non-main live
  subagents can open observation views. This is presentation only; Roster remains the team-scoped
  directory of live agents.
- This implementation keeps one visible observation view active at a time. Multiple opened
  observations are modeled as tabs; a requested split shows the selected observation beside the
  main view rather than introducing a nested split tree.
- Closing or auto-closing a focused observation view returns focus to the main view unless another
  opened observation is selected by the existing cycle/focus controls.
- The main agent view is globally unique and lives in the main tab. Other tabs/splits may observe
  subagents, but they do not duplicate the main Prompt composer or main transcript viewport.
- The manager overlay rebuilds from the current Team projection while open. If the selected row is
  absent from a refreshed item set, selection falls back through the dialog's normal clamped
  position.
- If no row is selected at commit, no observation view is created.
- Observation layout state is scoped to the active main Session. Switching/resuming another main
  Session closes existing observation views and resets to that Session's main tab.
- Observation layout state is not persisted. Restarting the TUI or resuming a Session starts at the
  main tab; users reopen live subagent observation views through the manager when needed.
- Ctrl+X O replaces the existing Team Roster overlay with the Subagent manager: same live Roster
  projection, extended with open-as-tab/open-as-split actions. Escape closes it; confirming a
  selection opens/focuses the requested observation view and returns to the Session screen.
- The Subagent manager is observe-only: it opens, focuses, and closes observation views and filters
  the Roster. It does not kill, restart, reassign, or send work to subagents.
- If no live subagents are selectable, Ctrl+X O still opens the manager with a clear empty state;
  no tab or split is created until a live subagent is selected.
- The main view remains quiet by default but exposes a compact status/count indicator when live
  subagents exist or need attention, showing total live subagents plus attention counts such as
  blocked/permission/question states; Ctrl+X O is the path from that indicator to details.
- The manager supports `/`-entered filtering over handle, agent type, and current task while
  preserving arrow/page navigation through the filtered rows. Outside filter mode, `Enter`, `v`, and
  `s` are placement actions; Escape exits filter mode first, then closes the manager.
