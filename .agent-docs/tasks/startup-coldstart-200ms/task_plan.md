Project: /mnt/nvme0n1/yanweiye/Projects/hya
Phase: executing
Step: execute
Outcome: active

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | Bootstrap: lazy session metadata (no load_session×N) | — | done | coordinator | RED then GREEN test; bootstrap ≪1s on big DB |
| 2 | TUI: solid-plugin bundle/compile + launcher prefers it | — | done | coordinator | hya-ts launches dist/compiled; F0 bun_entry ≪1.7s |
| 3 | Overlap Bun spawn with backend listen | 2 | done | coordinator | backend wait overlaps TUI load (default on) |
| 4 | Narrow workflow recovery (skip idle session replay) | 1 | done | coordinator | big-DB listen ≪666ms; recovery tests green |
| 5 | Re-profile F0+realistic; document vs 200ms | 1,2,3,4 | done | coordinator | findings with p50 numbers |
| 6 | Defer heavy TUI providers / minimal first paint | 5 | pending | coordinator | cut sync→shell ~280ms gap |
| 7 | Package ships `dist/` | 2 | pending | coordinator | install path never falls back to src transpile |
