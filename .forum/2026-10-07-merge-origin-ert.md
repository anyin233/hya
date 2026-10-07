# How was the old upstream ert test commit reconciled?

**Question:** Does merging anyin233/hya:ert replace the newer local pane features?

**Answer:** No. Upstream ert ended at test-only commit `5dfaa7f6`, branching from `fc92319f`; local ert continued through `beadaec7`. The merge preserves both ancestries and all shipped frontend/backend files from the newer local tree. Current test equivalents supersede old assertions about fixed backgrounds, removed headings/shortcuts and outlined tool cards. Compatible incoming initial-session readiness and command synchronization remain, without removing current regression assertions.

The theme browser test must wait for the actual `─Theme` picker heading to disappear, rather than the obsolete `Theme ·` string. Otherwise Esc can still be pending when the next slash command is typed into the old filter. Wait for the highlighted Light row before Enter as well.
