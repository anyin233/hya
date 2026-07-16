# TUI session reset and subagent visibility

Status: accepted

The TUI treats `/new` as a clean Session-screen reset: it asynchronously aborts the old active Turn, clears local prompt bookkeeping, navigates immediately, and lazily creates the next persisted Session only when the next prompt is submitted. Queued prompts mean only prompts held before backend readiness; submitted/in-flight prompts are not queued.

Subagent visibility stays derived from team/member events rather than synthetic transcript Messages. The live TUI timeline retains compact subagent activity rows only for failed or cancelled terminal outcomes; successful lifecycle events remain represented by the existing tool-call row without a duplicate activity row. The sidebar shows busy or attention-needed named Roster entries; the Subagent manager remains the full team inspection surface. Copy/export continue to use the stored Message transcript only.

Considered alternatives: storing synthetic System messages would make exports show subagent lifecycle, but would duplicate event data and blur Message history with Member lifecycle. Waiting for `/new` abort confirmation would give a stronger cancellation guarantee, but would make a slow backend block navigation.
