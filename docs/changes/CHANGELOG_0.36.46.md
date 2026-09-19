# 0.36.46

## Compat endpoints verified gone; old-TUI verification removed; documentation fully updated (repo-wide)

- Endpoint audit (subagent-verified): zero legacy route registrations
  remain anywhere in `crates/` — every route is `/v1/...` from the
  `hya-server::v1` router plus the gRPC binding through the same router.
  Stale doc comments referencing the deleted modules were cleaned.
- Old-TUI compatibility is now deliberately and completely broken, and
  its verification is removed with it: the seven backend-integration
  bun suites that drove the vendored TUI against deleted endpoints
  (`real-backend`, `real-backend-agents`, `workflow-pty`, `pty-smoke`,
  `sdk-spine`, `agent-model-sync`, `coding-tool-sync`) are deleted; the
  legacy `hya-sdk` endpoint-verifying unit tests and the
  `backend_spike` example hitting `/config` + `/global/event` are gone;
  the `hya` crate drops its unused `hya-sdk` dev-dependency. The TUI
  source stays compiling (`tsgo --noEmit` clean); new-TUI coverage on
  `hya-sdk-v1` returns with that frontend.
- Test infrastructure aligned: the e2e matrix retires the deleted Track
  T real-backend scenarios and compat Track I rows (replaced by
  `I.v1_api` + `I.v1_grpc_parity`; `xtask matrix-check` green), and the
  CI Track T gate now runs the surviving registered suite only.
- Documentation fully updated (40+ pages): architecture pages, testing
  guides, spec pages, configuration/CLI/troubleshooting, project
  structure, README, AGENTS.md — every stale reference to the deleted
  surface is rewritten to its v1 equivalent, marked historical, or the
  section removed; `compat-parity.md` is banner-marked as a historical
  record. Living CLI no-op flags, the internal event enum, the plugin
  JSON-RPC adapter, and `ServerHandle` supervision are deliberately
  preserved as documented living surface.
- Gates: fmt clean; workspace clippy `-D warnings` zero; workspace
  tests green (two known load-flakes in the concurrent workstream's
  hya-app/hya-tool timing tests pass isolated and in-crate); backend
  binary builds; TUI package typechecks.
