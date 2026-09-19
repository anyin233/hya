# 0.36.45

## Every remaining consumer runs on the v1 contract (app, backend, native, hya)

- Consumer sweep after the Compat deletion, all green on the v1-only
  surface:
  - `hya` (launcher): the in-process accept-gate test drives the full
    v1 lifecycle (create session, event-driven turn to terminal state)
    through tower oneshot against the router — still asserting ZERO
    loopback sockets — replacing the retired native-SDK round trip.
  - `hya-app`: the nested-spawn tree tests read the team roster from
    the shared projection instead of the deleted `/session/{id}/tree`
    route.
  - `hya-backend`: the LSP runtime test reads `/v1/fs/symbols`.
  - `hya-native`: the legacy Compat event bridge (`spawn_event_bridge`,
    which spoke the retired `server.connected` SSE schema) is deleted;
    the transport tests drive `/v1/health`. The `hya` crate drops its
    unused dependency; what remains is the in-process transport itself
    (old TUI only, until the new-TUI cutover).
- Final gates on the branch: fmt clean; `cargo clippy --workspace
  --all-targets -D warnings` zero errors; workspace tests (excluding
  the in-flight orchestration crates owned by the concurrent
  workstream) 1306 passed / 0 failed; process e2e matrix 43/43.
