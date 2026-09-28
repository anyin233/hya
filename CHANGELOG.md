# 0.43.2

## Compaction preserves in-flight turns

- Compaction markers now sit before the retained tail in the event-sourced projection, so a mid-turn compaction keeps the active assistant's prior tool calls and results in later model requests instead of restarting the task.
- The compaction ladder excludes the marker and active assistant tail from the foldable range, preventing repeated compaction of the same boundary.
- Added regression coverage for marker placement and post-compaction request reconstruction; the compaction reference documents the boundary contract.
