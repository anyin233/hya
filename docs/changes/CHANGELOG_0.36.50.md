# 0.36.50

## The API consolidation merges onto main: hya.v1 is the only contract, on both transports (repo-wide)

Merges the `api-consolidation` branch (11 releases, 0.36.39-api-branch
through 0.36.49 — archived under `docs/changes/`) into main after the
subagent orchestration redesign. The combined surface:

- **One contract, two transports**: the `hya.v1` IDL (16 services, 79
  rpcs) served over HTTP/JSON+SSE+WebSocket (`/v1`) and gRPC
  (`V1Grpc` through the same router; `HYA_GRPC_BIND`), with a
  dual-transport parity suite. The Compat surface (v2 `/api/*`, legacy
  bare paths, `/tui/*`, old native `/sessions/*`) is deleted; shared
  machinery lives under `hya-server::support`.
- **Event-driven execution**: `CreateTurn` admits and returns a handle;
  progress arrives on the streams; transcript/todo reads fold the shared
  projection. Interaction frames (`permissionRequested`,
  `questionRequested`, `interactionResolved`) are live on both streams,
  and "always" permission answers persist saved rules.
- **Agent-model preferences over v1** (`/v1/agent-models`) with the
  tier chain (session > configured > remembered > default), plus
  **dispatch-time model resolution** for subagent spawns: exact id →
  substring fallback (bare vendor ids excluded) → user configuration.
- **New frontends**: `hya-sdk-v1` (typed client + SSE + session mirror),
  `hya-client` on `/v1`; the e2e matrix (p01–p22) drives real backends
  entirely through the v1 client. The old TUI's backend-integration
  verification is retired with the deleted surface.
- Merge resolution notes: the orchestration redesign's relocated spawn
  machinery wins structurally; the dispatch-model resolution hook is
  re-applied at the new `resolve_authorized_spawn_member` site; the
  redesign's `MailConsumed` compat-event fix is superseded by the
  surface deletion (the curated stream maps it to no frame). Branch-side
  changelogs 0.36.39–0.36.48 are archived under their numbers (the
  colliding branch 0.36.39 as `CHANGELOG_0.36.39-api-branch.md`).
- e2e harness stabilized for the unified resident substrate: the
  session-tree builder walks per-session `members` rows (mirroring the
  projection run-tree) instead of parent-filtered listings, and the
  resident-scheduling scenarios (p08/p09/p21/p22) pin each
  conversation's script with system-prompt-marked routes — the quiesce
  steer can issue extra parent rounds, so shared script queues and
  positional request indexing were nondeterministic by construction.
  The e2e matrix (minus the pre-existing p20 catalog breakage on main)
  is 40/40 green; the workspace suite is 1635 passed / 0 failed.
