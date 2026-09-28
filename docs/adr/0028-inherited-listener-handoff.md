# ADR-0028: Supervisor-owned listener handoff

Date: 2026-09-28
Status: Accepted for the first implementation slice

## Context

The backend daemon currently binds its listener and `hya serve restart` stops the
old process before starting the new one. The updater can stage and verify an
immutable generation, but the process transition still has a port-availability
gap and must reconstruct the server from the durable database.

Rust code must not be hot-patched inside the long-lived engine. A supervisor-owned
listener is a lower-risk primitive: the listener lifetime is outside the server
process, while the server binary and runtime state can be replaced at a process
boundary.

## Decision

Foreground `hya serve` accepts `--listen-fd <FD>` on Unix. The supplied descriptor
must be at least 3 and must already refer to a TCP listening socket. hya takes
ownership, sets `FD_CLOEXEC`, switches it to nonblocking mode, adopts it as a
Tokio `TcpListener`, and runs the same HTTP/JSON/SSE/WebSocket/gRPC server loop
used by normal binding.

The inherited path is fail-closed:

- an invalid descriptor is an error;
- hya never silently falls back to `--bind`;
- standard streams are rejected;
- `--listen-fd` conflicts with `--bind`, `--hostname`, `--port`, and `--mdns`;
- daemon control actions (`start`, `status`, `stop`, `restart`) reject the option.

The listener handoff does not migrate application state. SSE/WebSocket streams,
in-flight turns, event cursors, and relay sessions retain the existing drain,
`serverStopping`, reconnect, and event-log recovery behavior.

## Consequences

### Positive

- A supervisor can keep the listening socket bound while replacing the foreground
  server process.
- The steady-state request path is unchanged: no dynamic Rust call indirection,
  serialization layer, or ABI boundary is added.
- Host allowlist, relay setup, database lock, discovery publication, health, and
  graceful drain continue through the existing `prepare_server` and `Server::serve`
  path. Non-loopback host names require explicit `--allow-host` flags.
- A wrapper can provide descriptors from an external supervisor. Native systemd
  environment-variable discovery and launchd socket activation are not implemented;
  neither supervisor is required for the normal CLI.

### Limitations

- This slice does not make `hya serve restart` perform successor handoff.
- The database lock still belongs to one process; successor lock transfer needs a
  separate protocol and must not be inferred from FD inheritance.
- Existing HTTP streams are not promised to survive process replacement.
- A descriptor is a capability supplied by the process supervisor; accepting it
  does not sandbox the server or its child processes.

## Follow-up

1. Add a supervisor/successor protocol that transfers listener ownership and the
   database lock without a stop-then-start gap.
2. Add generation and activation fences so old and new daemons cannot both append
   to the same event log.
3. Exercise health/bootstrap/first-turn recovery and SSE/gRPC reconnect in process
   E2E tests.
4. Add Linux systemd and macOS launchd packaging adapters only after the portable
   foreground primitive is proven.
5. Keep agent self-patching limited to proposal, isolated build, verification, and
   staging; owner/supervisor activation remains outside the agent authority.
