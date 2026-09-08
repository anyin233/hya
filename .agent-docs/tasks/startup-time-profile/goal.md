# Goal

Profile interactive `hya` cold start from process entry to TUI shell paint / sync complete.

## Scope
- Measure release-path waterfall with `HYA_STARTUP_TRACE`
- Identify sequential work that can overlap asynchronously
- Identify work that can be deferred/lazy-loaded past first paint / sync-complete

## Non-goals
- Implementing optimizations in this task (analysis + evidence only)
- Changing product budgets

## Acceptance
- Wall-clock numbers for key marks (p50/p95 or multi-run samples)
- Critical-path breakdown with overlap and lazy-load candidates quantified
