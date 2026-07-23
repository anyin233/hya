# F0 baseline (Gate 0 partial)

Machine: development host (Linux). Date: 2026-07-23.

## Backend ready (release `hya-backend serve`, empty temp DB, no MCP)

Command:

```sh
cargo run -p xtask --release -- startup-bench --mode backend --runs 5
```

Results:

| run | backend_ready_ms |
| --- | --- |
| 1 | 126.2 |
| 2 | 120.5 |
| 3 | 128.2 |
| 4 | 126.2 |
| 5 | 122.9 |

- **p50** ≈ **126.2 ms**
- **p95** ≈ **128.2 ms**

Notes:

- Already ~25% of the 500 ms full-sync budget before Bun or TUI bootstrap starts.
- `HYA_STARTUP_TRACE=1` emits `backend_listen` marks from `hya-backend` / `hya-ts`.
- Full E2E (shell paint + sync complete) still requires a PTY-capable harness; TUI marks are wired for interactive runs with `HYA_STARTUP_TRACE=1`.

## Residual budget ledger (F0 sketch)

| Slice | Budget allocation (draft) | Measured floor |
| --- | --- | --- |
| backend listen | ≤ 150 ms | ~126 ms |
| Bun → shell paint | ≤ 100 ms | TBD (needs PTY / interactive) |
| blocking + complete bootstrap | ≤ 200 ms | TBD |
| slack | ≤ 50 ms | — |

## Instrumentation shipped (WP-0)

| Mark | Emitter |
| --- | --- |
| `hya_ts_start` | `hya-ts` |
| `backend_spawn` / `backend_listen` | `hya-ts` (parent wall after spawn ready) |
| `backend_listen` | `hya-backend` serve |
| `bun_entry` | `packages/hya-tui-ts/src/main.tsx` |
| `theme_resolved` | `app.tsx` |
| `plugin_host_done` / `shell_paint` | `app.tsx` (currently after plugins; will move with WP-2) |
| `sync_partial` / `sync_complete` | `sync.tsx` |
