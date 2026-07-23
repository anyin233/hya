# F0 baseline (Gate 0 → after WP-1)

Machine: development host (Linux). Date: 2026-07-23.

## Backend ready (release `hya-backend serve`)

Command:

```sh
cargo run -p xtask --release -- startup-bench --mode backend --runs 5
```

### Before WP-1 (MCP/plugins blocked listen)

| run | backend_ready_ms |
| --- | --- |
| 1–5 | ~120–128 |

- **p50** ≈ **126 ms**, **p95** ≈ **128 ms**

### After WP-1 (MCP deferred by default)

| run | backend_ready_ms | mark_delta_ms |
| --- | --- | --- |
| 1 | 8.5 | 8.0 |
| 2 | 8.0 | 8.0 |
| 3 | 8.5 | 9.0 |
| 4 | 8.0 | 8.0 |
| 5 | 8.6 | 8.0 |

- **p50** ≈ **8.5 ms**, **p95** ≈ **8.6 ms**
- Slow MCP no longer blocks listen (`McpStatus::Connecting` until attach finishes; tools hot-register).

Notes:

- `HYA_STARTUP_TRACE=1` emits marks; restore classic path with `HYA_DEFER_SIDEPLANES=0`.
- Full E2E (shell paint + sync complete) still needs a PTY-capable harness.

## Residual budget ledger (updated)

| Slice | Budget allocation | Measured floor |
| --- | --- | --- |
| backend listen | ≤ 150 ms | **~8.5 ms** (WP-1) |
| Bun → shell paint | ≤ 100 ms | TBD (PTY / interactive) |
| blocking + complete bootstrap | ≤ 200 ms | **~22 ms** single `/tui/bootstrap` (WP-4); multi-call ~17 ms warm parallel |
| slack | remainder | — |

### WP-4 bootstrap payload

| Endpoint | Bytes | Notes |
| --- | --- | --- |
| `/command` (full) | ~258 KB | includes full skill templates |
| `/tui/bootstrap` | ~108 KB | commands omit templates; one RTT for full sync fields |

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
