Project: /mnt/nvme0n1/yanweiye/Projects/hya
Phase: executing
Step: commit
Outcome: active

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | Image transport repair | — | done | ImageRepair | Actual provider gets image bytes on live/replayed request |
| 2 | Escape and heap UI repairs | — | done | TuiRepair | PTY abort and real heap output |
| 3 | Builtin skill catalog repair | — | done | SkillRepair | Advertised builtin loads through tool |
| 4 | Runtime LSP implementation | — | done | Main | Real server symbol/definition/diagnostics lifecycle |
| 5 | Release metadata and docs | 1,2,3,4 | in_progress | Main | Version bump, current-only changelog, accurate docs |
| 6 | Full gates and live acceptance | 1,2,3,4,5 | done | Main | All gates and original scenarios pass |
| 7 | Atomic commits and final records | 6 | in_progress | Main | Scoped commits and clean tree |
