# How do selectable and passive panes share the TUI?

**Answer:** Every built-in kind has state/panes.ts metadata and a components/paneRegistry.tsx render/input registration. Only visible selectable leaves own input or participate in focus navigation. Passive viewer, activity, todos, context and status stay in the layout and allow mouse interaction without taking focus. PaneWorkspace installs the single input subscription; mounted selectable panes share PaneInputHandle. The editor currently registers existing global/overlay dispatch and its local handler separately. There is no external plugin loader yet.

Layout version 3 separates conversation/viewer and composer/editor. Keep the legacy conversation id on the editor and allocate fresh viewer AND activity ids; omitting activity silently loses heartbeat for existing saved layouts. A one-owner subtree navigates together with passive siblings to preserve Sessions→Left→editor. Projects focus is now derived from paneLayout.active. Passive-pane targeting and editor/viewer closing/reachability are later work at the user's request.

Verify browser rendering through packages/hya-tui-web. New regression: e2e/hya-tui-selectable-panes.spec.ts covers passive clicks, input isolation, rotation, narrow layout and migrated heartbeat. Artifacts belong under ~/data.

## Does a box imply keyboard selectability?

**Answer:** Yes. PaneFrame reads paneDefinitions[kind].selectable and draws an enclosing border only for selectable workspace panes. Passive Todos, Context and Status retain plain labels on the base background. MainPanel non-chat output is also passive and borderless. This avoids a visual promise of focus navigation on a passive pane. Content widgets inside panes retain their own rendering contracts.

**OpenTUI pitfall:** `border: false` plus a `borderColor` auto-enables borders. Use `border: []` for passive frames, including reactive transitions from a selectable kind. Verify full frame edges in browser terminal cells, not only the absence of a title rule.
