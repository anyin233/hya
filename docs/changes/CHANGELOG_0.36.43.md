# 0.36.43

## The Compat surface is gone: `/v1` is the only HTTP contract (server)

- Deleted the entire legacy route surface: all Compat route groups
  (~63 modules), the three `/session`-family mirrors, the legacy
  `/tui` control plane, `/doc`+`/openapi.json`, the old native
  `/sessions/*` routes, and the legacy `/event`-family SSE endpoints.
  The server now serves exactly one contract: `hya.v1` over
  HTTP/JSON+SSE+WebSocket (and gRPC via `V1Grpc` through the same
  router). 86 legacy integration-test files were removed with it.
- The shared machinery those routes used survives re-homed under
  `hya-server::support` (unchanged logic, route handlers stripped):
  command/skill catalogs with template expansion, bound-agent
  resolution and AGENTS/reference guidance, PTY runtime, worktree git
  helpers, the VCS git module, the config bag, and the JSONC/model-ref
  utilities. Pending permission/question planes keep their full
  bridges for the interaction stream.
- v1 fixes surfaced by the deletion sweep: MCP duplicate-tool
  collisions map to `unavailable` (503) instead of `internal`; MCP
  add/connect bodies use the inlined oneof (`command: {...}`); skill
  listings and MCP status reads in the e2e harness moved to `/v1`.
- Gates: server suite 26/26 green; the full process e2e matrix (p01–
  p20, minus the pre-existing main-broken catalog test) 43/43 green
  against real backends running the v1-only surface.
