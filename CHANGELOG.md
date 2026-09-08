# 0.36.24

## Cold-start performance

- `/tui/bootstrap` returns empty `sessions` and skips full event-log replay; the TUI lists sessions after first paint.
- Prefer prebundled TUI entry (`packages/hya-tui-ts/dist/boot.js`) when present; set `HYA_TUI_ENTRY=src` to force source.
- Owned mode overlaps Bun load with backend listen by default (FIFO URL handoff); set `HYA_STARTUP_OVERLAP=0` to disable.
- Startup Workflow recovery only replays Sessions that contain `workflow_run_started`, so large idle DBs no longer dominate `backend_listen`.
- Vendor `sevenz-rust2` builds as `rlib` only to avoid Cargo output-filename collisions that broke release builds.

