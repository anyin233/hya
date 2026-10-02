# How should ert preserve its TUI features when merging main b37db242?

**Answer:** Retain the v4 ordered layout tree, Layout editor pane, passive/selectable contract, stable mounts, strict input routing, configurable keybindings and heading-free conversation. Port upstream pointer menus into the existing column-capable Picker, and show frontend/backend versions in the existing Context and Status panes. Keep automatic saved-chat restoration even though upstream plain startup creates a fresh chat. Browser launch coverage explicitly checks restoration.

Upstream now uses versions.toml as the backend/frontend release source, separate changelogs and frontend-version.ts. This merge has backend 0.45.2 / frontend 0.44.6, minimum backend 0.43.41. Internal manifests/bundles use version references, not the old coupled version bump. Old root changelog retained under docs/changes.

## Why did wide-screen browser tests fail after the merge?

**Answer:** Context acquired a Version row. The shared showStatusView helper treated it as evidence that /status was already open. Match the explicit Status field spacing before reading full session ids or Directory. Browser regression hya-tui-sidebar-context.spec.ts covers both pointer menus, draft isolation and keybinding columns.

## Is the imported daemon restart fully verified?

**Answer:** Nine daemon process tests pass, including ordinary restart readiness. The upstream self-restart test fails: the agent's shell runs blocking serve restart, but handoff waits for that same active turn to reach a safe boundary. The journal records a failed handoff after the deadline. The daemon implementation and test are identical to main b37db242 (change introduced by 4a75fe9d). This merge leaves upstream backend behavior intact; the self-restart failure needs a separate fix. Evidence is recorded in .planning/2026-10-02-merge-main and ~/data/hya-rust/tmp/merge-main-daemon.log.
