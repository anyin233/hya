# ADR-0028: Supervisor-owned listener handoff

Date: 2026-09-28
Status: Accepted

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

## Successor daemon handoff

`hya serve restart` uses the same capability boundary internally. The running
daemon duplicates its TCP listener, optional extra gRPC listener, database
`flock`, and handoff journal inputs, clears close-on-exec on the duplicates only
for the child spawn, and starts a successor with `--listen-fd`, `--lock-fd`,
`--handoff-journal`, `--inherit-status`, and (when configured)
`--grpc-listen-fd`. The old process remains the discovery owner until the
successor has composed the runtime, resumed durable handoff sessions, published
discovery, and answered `/v1/health`.

The predecessor enters an engine-wide admission quiesce before the handoff.
Existing turns and permission asks remain serviceable for the drain deadline;
new turns are refused. A root turn that reaches its next safe round boundary
closes with the durable `FinishCause::Handoff` marker and one pending-resume
row. A turn still active at the deadline is a straggler: the handoff aborts,
the quiesce lifts, and the old generation keeps serving without terminalizing
that turn. The successor resumes only root, non-workflow sessions whose folded
transcript still ends at the marker. The tail predicate is the at-most-once
fence: a later prompt or continuation makes the session ineligible. Running child/member turns are not cutover boundaries: they keep their lease and
make the handoff reject if still active at the deadline; idle members are revived
by successor resident recovery. Running Workflow sessions likewise reject the
handoff rather than being transferred. Normal stop and crash recovery retain their
existing `shutdown` and `interrupted` causes.
The internal successor interface is:

| Option | Contract |
| --- | --- |
| `--listen-fd <FD>` | Existing listening IPv4/IPv6 TCP socket; fail closed if invalid. |
| `--lock-fd <FD>` | Existing database flock paired with the listener. |
| `--handoff-journal <PATH>` | Handoff journal used to wait for predecessor `released` and record successor stages. |
| `--inherit-status <MILLIS>` | Predecessor discovery timestamp retained by the successor. |
| `--grpc-listen-fd <FD>` | Optional inherited extra gRPC listener. |

If successor composition or health fails, the predecessor keeps the lock and
listener parked as the recoverable owner and records an explicit failure. The
restart controller does not silently start a second generation while that
owner remains healthy. Forced termination remains an explicit operator action.

Client SSE/WebSocket streams receive `serverStopping {reason: "restart"}` and
reconnect to the same URL. Durable sessions, event cursors, catalogs, todos,
and pending interaction listings are re-read from the successor projection;
process-local permission oneshots are intentionally not serialized across the
boundary.


The external foreground primitive transfers only the listener. The daemon
successor path additionally transfers the database lock and rebuilds application
state from the durable store; client streams still reconnect at the process
boundary.

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

- Existing HTTP streams are not promised to survive process replacement; clients
  must process `serverStopping` and resubscribe.
- Process-local permission oneshots cannot be serialized. The old generation
  waits for them through the handoff deadline; if the ask keeps its turn active,
  the handoff is rejected and no durable close is fabricated.
- Native systemd environment-variable discovery and launchd socket activation
  are not implemented; neither supervisor is required for the normal CLI.
- A descriptor is a capability supplied by the process supervisor; accepting it
  does not sandbox the server or its child processes.

## Follow-up

1. Add systemd/launchd packaging adapters only after the portable successor
   primitive is proven in process E2E.
2. Add updater generation fencing and owner authorization for self-iteration;
   the agent may propose and verify, but activation remains supervisor-owned.
3. Extend process E2E coverage to a provider stream, a shell-invoked restart,
   and successor bootstrap failure with fallback recovery.
