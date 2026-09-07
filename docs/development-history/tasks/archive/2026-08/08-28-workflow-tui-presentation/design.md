# Design

The operator explicitly requires sidebar state. The parent design section **TypeScript TUI** is authoritative.

## State Seam

The existing SDKProvider and SyncProvider remain the only transport and state modules. Bootstrap/Session DTOs contain typed Workflow state; existing `session.updated` events replace it. The TUI never folds raw Workflow Events and never polls.

## View Seam

Register one hya-owned built-in through `sidebar_content`. It is a read-only adapter over synchronized Workflow state and existing theme/width interfaces. It does not add a roster sidebar, prompt status line, Workflow provider, or graph renderer.

The server supplies derived activity joined to Workflow Member references. The view only orders, labels, colors, and truncates it. Existing run-tree and roster modules remain the Agent navigation surface.

## Presentation

Display selected identity/availability, run state, active/total Agent instances, Stage/level progress, active Stage ids, and bounded current work. Identity/count fields have truncation priority over work. Fan-out uses deterministic `first +N` compact form. No selection remains visible as muted `Workflow: none`.
