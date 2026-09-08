Project: /mnt/nvme0n1/yanweiye/Projects/hya
Phase: finalizing
Step: none
Outcome: complete

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | Map startup critical path & existing instrumentation | — | done | coordinator | Mark inventory + process chain documented |
| 2 | Build release binaries and run backend startup-bench | 1 | done | coordinator | backend_ready p50/p95 numbers |
| 3 | Capture full interactive waterfall (hya→shell→sync) | 2 | done | coordinator | Multi-run mark deltas from hya_ts_start to sync_complete |
| 4 | Analyze overlap + lazy-load opportunities | 3 | done | coordinator | Quantified recommendations with evidence |
| 5 | Commit analysis artifacts under task dir | 4 | skipped | coordinator | User commit rule: no commit unless asked; artifacts remain in `.agent-docs/tasks/startup-time-profile/` |
