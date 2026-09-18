# 0.36.34

## Added the `hya.v1` dual-protocol API contract: IDL, codegen, and generated docs (api)

- The new `proto/hya/v1` directory is the single source of truth for the
  consolidated API: 15 services and 77 rpcs covering process/config/
  bootstrap, catalogs, provider auth, sessions, event-driven turns,
  messages/todos, replay+SSE/gRPC event streams, the unified
  permission/question interaction plane, workflow state/commands,
  filesystem reads, project/VCS, worktrees, MCP, PTY (including the
  bidirectional `StreamPty`), and log ingest. Every rpc and field carries
  documentation comments, and every rpc declares its HTTP binding via the
  `// hya.http:` convention.
- `crates/hya-api` is the contract crate: generated prost types, tonic
  clients/servers, canonical protojson (pbjson) serde, the stable error
  code table mapped identically to HTTP statuses and gRPC status codes,
  and opaque pagination cursor helpers. Generated output is committed, so
  normal builds and CI never need `protoc`.
- `cargo run -p xtask -- gen-api` regenerates everything from the IDL
  using a vendored protoc (no system dependency): Rust codegen plus
  `docs/protocol/api-reference.md` and `docs/protocol/openapi.json` for
  third-party TUI/GUI/WebUI integrations. The task fails when any rpc is
  missing its HTTP mapping or when two rpcs claim the same route, keeping
  the dual-protocol parity promise checkable rather than aspirational.
- This lands phase P1 of the API consolidation plan: no runtime behavior
  changed yet. The HTTP `/v1` binding (P2) and the gRPC listener (P3)
  build on this crate; the legacy surface is untouched until the cutover.
