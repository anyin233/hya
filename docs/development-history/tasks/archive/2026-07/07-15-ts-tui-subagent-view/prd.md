# 实现 TS TUI Subagent View

## Goal

Replace the unusable OpenCode-style subagent viewer in the TypeScript TUI with a native subagent workspace that makes concurrent and nested agent activity inspectable from one screen.

## Background

- The Rust implementation is the design reference for the new TypeScript TUI experience.
- The current TypeScript TUI subagent view is reported as unusable.
- The existing `Ctrl+X`, then `Down` OpenCode-style viewer must be removed rather than retained as a second path.
- The project design system already defines subagent observation as a tabbed/split-pane view without a Prompt composer (`DESIGN.md:75-77,103-106`).
- The Rust TUI already defines the native manager/pane commands and serves as the interaction reference: roster, tab selection, vertical/horizontal split, close, focus cycle, and focus main.
- ADR 0003 defines the single-input invariant: the main view is globally unique and uncloseable; Prompt, permission, and question input remain main-owned, while observation panes are read-only.
- Existing backend/SDK contracts already expose recursive run lineage and live member/session state; the UI must consume those authoritative projections rather than reconstructing a competing hierarchy.
- Accepted ADR 0006 requires subagent visibility to derive from team/member events and keeps lifecycle state out of synthetic transcript messages.

## Requirements

- The TypeScript TUI provides a subagent selector rendered as a parent/child tree.
- The selected workspace can show different agents side by side in tmux-like split panels on one page.
- Each tab supports repeated horizontal and vertical splits, allowing two or more different subagents to remain visible simultaneously.
- Users can open multiple tabs, each displaying subagent state.
- The TypeScript TUI provides the Rust-equivalent manager and pane command set with configurable keybindings: `<leader>o`, `<leader>T`, `<leader>V`, `<leader>S`, `<leader>w`, `<leader>.`, and `<leader>0` by default.
- Manager actions use `Enter` for a tab, `v` for a vertical split, and `s` for a horizontal split; direct placement commands open the same selector with the placement preselected.
- A subagent session owns at most one observation pane across the active workspace; selecting it again focuses its existing pane instead of duplicating it.
- The main pane is unique and uncloseable. Observation panes never render a Prompt composer or accept text/Enter as agent input; permission/question prompts remain main-owned.
- Observation layout resets when the main session changes and is not persisted across TUI restarts.
- Selector filtering covers handle/agent type/current task, and the root/main row is visible but not selectable when present.
- Observation panels follow the borderless tonal layout and width-safe text rules in `DESIGN.md`.
- Each visible agent view presents that agent's current state using the runtime's authoritative state source.
- A terminal-state pane closes automatically unless it is focused; a focused terminal pane remains inspectable until focus moves away, then closes.
- Nested subagents can be spawned and represented under their spawning agent.
- Existing recursion, concurrency, and per-run governor limits remain authoritative for nested spawning.
- The OpenCode-style `Ctrl+X`, then `Down` subagent viewer and its obsolete entry path are removed.
- Legacy task hints, whole-route child navigation commands, and the read-only child footer are removed when superseded by the native workspace; no duplicate viewer remains.
- Copy/export behavior remains based on stored message transcripts and does not gain synthetic subagent lifecycle messages.
- Existing non-subagent TUI workflows continue to work.

## Acceptance Criteria

- [ ] **AC-01:** A user can open the native TypeScript TUI subagent workspace without using `Ctrl+X`, then `Down`.
- [ ] **AC-02:** The selector renders root agents and nested descendants in the correct tree relationship.
- [ ] **AC-03:** A nested subagent spawned by another subagent appears under the correct parent and can itself spawn a child when runtime policy allows it.
- [ ] **AC-04:** An executable cross-layer test proves that a child-session spawn is returned as a grandchild by the run-tree contract; no duplicate spawning mechanism is introduced.
- [ ] **AC-05:** One workspace page can display at least two different agents concurrently in separate panels without losing either panel's state updates.
- [ ] **AC-06:** Repeated splits build a stable pane layout within the active tab; focus and close operations target one pane without corrupting the remaining layout.
- [ ] **AC-07:** A terminal-state pane closes automatically when unfocused; if terminal state arrives while focused, final state remains visible and the pane closes only after focus moves elsewhere.
- [ ] **AC-08:** A user can create, switch, and close multiple subagent tabs without corrupting the active panel or selector state.
- [ ] **AC-09:** The Rust-equivalent manager, tab, split, close, and focus commands are reachable through the TS keymap and expose their configured shortcuts in relevant UI hints.
- [ ] **AC-10:** `Enter`, `v`, and `s` commit the selected tree node to tab/vertical/horizontal placement; cancel or an empty/non-selectable row leaves layout unchanged.
- [ ] **AC-11:** Main Prompt text and submission target remain unchanged while any observation pane is focused; ordinary pane-focused text/Enter is ignored.
- [ ] **AC-12:** Opening an already observed subagent focuses its existing pane, and changing main session clears ephemeral observation tabs/layouts.
- [ ] **AC-13:** The observation workspace omits the Prompt composer while focused and uses immediate deterministic keyboard navigation.
- [ ] **AC-14:** The removed OpenCode-style shortcut no longer opens a subagent viewer and no obsolete duplicate viewer remains reachable.
- [ ] **AC-15:** Task-tool UI points to the native workspace and no longer advertises the removed child-session shortcut.
- [ ] **AC-16:** Subagent lifecycle/status rendering is driven by existing session/member/team projections; copy/export output remains unchanged.
- [ ] **AC-17:** Focus, navigation, terminal sizing, and state updates are covered by focused executable tests derived from the repository's existing TUI test patterns.
- [ ] **AC-18:** Required workspace formatting, linting, tests, and local executable build pass.

## Out Of Scope

- Product behavior unrelated to subagent inspection, selection, layout, tabs, or nested spawn.
- A second compatibility viewer alongside the native TypeScript TUI view.

## Notes

- This task remains in planning until `design.md`, `implement.md`, and the context manifests are reviewed.
