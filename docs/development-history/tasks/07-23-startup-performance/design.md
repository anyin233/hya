# Design: startup progressive readiness

## Readiness plane

```text
core-ready  →  shell-ready  →  sync-complete  →  ext-ready
  (listen)      (≤100ms Bun)     (≤500ms hya)     (MCP/plugins)
```

| Plane | Contract |
| --- | --- |
| core-ready | Store + engine + builtins; HTTP listening |
| shell-ready | Shell chrome painted (budget B) |
| sync-complete | TUI bootstrap `status === "complete"` with snapshot data (budget A) |
| ext-ready | MCP/plugin children connected; tools hot-registered |

## Critical-path changes

1. **Backend:** split `build_session_engine` so `connect_all` for MCP/plugins runs after bind; hot-register tools; status endpoints return connecting/connected/failed immediately.
2. **TUI paint:** default theme without awaiting `waitForThemeMode(1000)`; split `shellReady` vs `pluginsReady`; paint Home before plugins finish.
3. **Sync:** keep `complete` semantics; ensure status endpoints are snapshots (no connect-on-read). Optional later: batch bootstrap endpoint.
4. **Supervisor:** keep await-listen by default after listen is cheap; optional parallel Bun only if ledger requires it.

## Trace contract

When `HYA_STARTUP_TRACE=1`, each process may emit JSON lines to stderr:

```json
{"hya_startup":true,"mark":"backend_listen","wall_ms":1710000000000,"detail":"http://127.0.0.1:1234"}
```

Harness computes deltas from parent `t0` wall clock and from first mark per process.

## Flags

| Flag | Default (target) | Classic restore |
| --- | --- | --- |
| `HYA_DEFER_SIDEPLANES` | on (after WP-1) | `=0` await connect_all before listen |
| `HYA_WAIT_THEME` | off | `=1` await theme mode before first paint |
| `HYA_SYNC_PLUGIN_START` | off | `=1` gate routes on plugin host |
| `HYA_STARTUP_TRACE` | off | n/a (instrumentation) |

## Non-goals (v1)

- `hya-native` as interactive default
- Single-process Bun+engine
- Redefining `complete` to skip mcp/lsp/vcs waves
