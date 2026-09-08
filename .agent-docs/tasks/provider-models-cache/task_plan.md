Project: /mnt/nvme0n1/yanweiye/Projects/hya
Phase: executing
Step: execute
Outcome: active

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | Cache module: read/write `models.yml.cache` beside config.yaml | — | done | agent | unit tests green |
| 2 | `config::load` uses cache for empty-models providers; no blocking discovery | 1 | done | agent | warm cache + dead URL still loads models |
| 3 | Background refresh discovers, writes cache, swaps engine catalog | 2 | done | agent | cache updated; SSE catalog.updated |
| 4 | Emit `catalog.updated` SSE; sync.tsx re-applies providers | 3 | done | agent | TUI store updates after refresh |
| 5 | Update quality-guidelines + changelog 0.36.25 | 2,3,4 | done | agent | docs + version aligned |
| 6 | Verify gates for touched crates | 5 | pending | agent | focused tests green; full CI optional |
