# 0.36.36

## HTTP `/v1` binding covers workflow, filesystem, project/VCS, worktrees, and MCP (server)

- Workflow: `GET /v1/workflows` lists discovered sources through the
  app-owned control handle, `GET/POST /v1/sessions/{id}/workflow` reads the
  projected state and submits typed commands (list/info/select/run) with
  the same admission semantics and stable error codes as the legacy
  mirrors.
- Filesystem: `GET /v1/fs/read|list|find|search|symbols` serve scoped
  reads under the directory scope with traversal rejection, bounded
  walks that skip VCS/build directories, glob-to-regex file finding,
  substring text search, and LSP-backed symbol search.
- Project and VCS: project registry/current/directories plus
  `init-git`, git status (branch/head/dirty files), raw diff, and patch
  apply reuse the shared git helpers.
- Worktrees graduate to `GET/POST/DELETE /v1/worktrees` and
  `POST /v1/worktrees/{id}/reset` over the engine's worktree helpers.
- MCP: `GET/POST /v1/mcp` and `:connect`/`:disconnect` map onto the
  app-owned MCP control handle (stdio transports; typed status). OAuth
  surfaces answer `unavailable` honestly until wired.
- Two integration tests extend `tests/v1_api.rs` (filesystem round trip
  with traversal rejection; project/vcs/mcp/workflow catalog probes).
  All `/v1` domains are now live over HTTP; the gRPC binding and the
  PTY/`StreamPty` surface land next.
