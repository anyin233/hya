# 0.44.23-dev.7

## Breaking changes

- Requires backend 0.46.0 or later. Upgrade the backend and follow its database and bundle reinstall instructions before connecting.
- Subagent views are read-only for execution: sending prompts or backend commands, undo/redo, and changing model, agent or effort are blocked. Steer or archive the subagent from its parent. The session picker refuses child deletion; archive the child or delete its root instead.
- The generic `/api` catalog no longer offers the unavailable `DisposeProcess`, `UpgradeProcess`, `StartOauth`, or `CompleteOauth` operations.

## Bundles

- Refresh in `/bundles` now asks the backend to refresh its bundle catalog, then reloads the list and TUI extensions. Progress, success and failures are shown in the view.

## Transcript and streaming

- Label prior handoff context as `resumed from handoff (generation …)` rather than as ordinary compaction.
- Show live tool input without requiring each streamed fragment to be durable. After a stream gap, discard incomplete input until the complete durable replacement arrives.
- Remove the retired `invalid` placeholder from built-in tool recognition.
