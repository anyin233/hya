# Startup performance budgets

## Goal

Make interactive `hya` meet hard release cold-start budgets so the product feels instant on a typical laptop.

## Budgets

| Metric | Budget | Clock start | Done when |
| --- | --- | --- | --- |
| Time-to-full-sync | ≤ 500ms | `hya` / `hya-ts` process start (owned mode) | TUI `sync.status === "complete"` |
| TUI shell first paint | ≤ 100ms | Bun process entry | First terminal frame with full shell chrome (placeholders OK) |

Environment: release binaries, cold process start, empty-ish project (fixture F0). External MCP hang must not block either budget.

## Requirements

- Emit a structured startup waterfall (`HYA_STARTUP_TRACE`) with marks for backend listen, Bun entry, theme resolve, shell paint, plugin host done, sync partial, and sync complete.
- Shell chrome paints without waiting for TUI builtin plugin host completion or a long theme-mode probe.
- Backend listens (prints base URL) without awaiting full MCP / plugin `connect_all`; side planes attach in the background with status snapshots.
- Full-sync remains `status === "complete"` (blocking + non-blocking bootstrap waves). MCP status may be `connecting` at complete time.
- Reproducible bench path for F0 (and slow-MCP F2 for listen-not-blocked).
- Reversible flags so classic blocking startup can be restored.

## Acceptance Criteria

- [ ] Startup trace marks available without a debug rebuild when `HYA_STARTUP_TRACE=1`.
- [ ] F0 release cold: shell first-paint p95 ≤ 100ms from Bun entry.
- [ ] F0 release cold: full-sync p95 ≤ 500ms from `hya` start to `sync.status === "complete"`.
- [ ] Slow MCP fixture: listen URL appears before MCP handshake finishes; full-sync does not hang on MCP connect.
- [ ] Bench harness documents both metrics; CI can run F0 report mode.
- [ ] `HYA_FAST_BOOT` is not used as the budget gate (must not fake sync readiness).

## Notes

- Sole interactive frontend remains `packages/hya-tui-ts`.
- Progressive readiness: core-ready → shell-ready → sync-complete → ext-ready (MCP/plugins best-effort).
