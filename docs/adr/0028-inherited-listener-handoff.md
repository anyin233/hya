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

The predecessor keeps pending permission and question requests in a durable interaction journal while entering admission quiesce. The request IDs and client payloads remain stable; the successor rebinds each request to its fresh interaction receiver before publishing discovery. New turns are refused. Existing turns reach a safe round boundary, and completed tool calls are never replayed. A root turn that reaches its next safe round boundary closes with the durable `FinishCause::Handoff` marker and one pending-resume row. A turn still active at the deadline is a straggler: the handoff aborts, the quiesce lifts, and the old generation keeps serving without terminalizing that turn. The successor resumes only root, non-workflow sessions whose folded transcript still ends at the marker. The tail predicate is the at-most-once fence: a later prompt or continuation makes the session ineligible. Running child/member turns are not cutover boundaries: they keep their lease and make the handoff reject if still active at the deadline; idle members are revived by successor resident recovery. Running Workflow sessions likewise reject the handoff rather than being transferred. Normal stop and crash recovery retain their existing `shutdown` and `interrupted` causes.
The internal successor interface and restart readiness remain unchanged; interaction transfer is part of the durable state handoff and never replays a completed tool side effect.
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

Client SSE/WebSocket streams receive `serverStopping {reason: "restart"}` and reconnect to the same URL. Durable sessions, event cursors, catalogs, todos, and pending interactions are re-read from the successor projection. Pending interaction IDs and payloads remain unchanged across the reconnect.


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

 - Existing HTTP streams are not promised to survive process replacement; clients must process `serverStopping` and resubscribe.
 - Pending permission and question requests are durable handoff state. A response is atomically claimed and resolved, so duplicate client replies are idempotent and no tool call is replayed.
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

## Amendment (2026-09-28): self-proof and rollback

A handoff replaces running code, so it must neither admit a build that cannot
compose nor leave the backend down when a build fails after admission.

**Self-proof gate.** `hya serve restart` first runs the successor
executable's `hya serve check --db <db> --json`, which composes the complete
runtime (strict configuration, providers, bundles, native tool libraries,
plugins, startup recovery) against a private `VACUUM INTO` snapshot of the
database without the live database's locks, then every `--verify <cmd>`. Any
failure refuses the restart before the handoff journal is written or the
daemon is signalled. `--exe <path>` names a successor other than the invoking
binary; it passes the same gate.

**Generation pinning.** A daemon (and every successor) copies its executable
and the native tool libraries it loaded into `<db>.server.gen/<pid>/`, in the
layout the loaders resolve from, and removes the copy on exit; copies of dead
pids are swept. A Cargo-layout build also copies the in-tree first-party
bundle sources it loaded to `<pin>/first-party/`; the rollback successor gets
`HYA_FIRST_PARTY_SOURCE_ROOT=<pin>/first-party` (every other successor has the
variable removed), which `first_party_source_root()` honors. The running file paths cannot serve as the fallback because a
rebuild or update replaces them in place.

**Rollback.** When the successor records `failed`, exits, or is not ready
within 90 s, the predecessor kills it, restores its owner pid on the lock,
writes a fresh journal chain `requested` (successor = pinned executable,
`rolledBackFrom` = the failure) → `queued` → `released`, and spawns the pinned
build with the staged listener and lock through the ordinary successor path.
If it becomes ready, the handoff completes and `hya serve status` reports the
rollback; otherwise the predecessor parks as before.

**Migration policy.** A build opens a database whose applied migrations
include versions it does not know (sqlx `ignore_missing`), so the pinned build
can serve a database the failed successor migrated. Checksums of known
migrations still must match, and migrations must remain additive.

## Amendment (2026-09-30): TUIs reload with the backend

A restart replaces the backend's code but left every attached TUI on the old
TUI code until the user quit `hya`. Now a TUI that attached to the successor
after `serverStopping {reason: "restart"}` also reloads itself
([tui.md](../tui.md#hot-update-after-hya-serve-restart)).

**Where the reload lives.** Inside `packages/hya-tui`: the process a host
starts is a small supervisor that runs the app as a child Bun process on the
same terminal and starts it again when the app exits with status 75 after
writing a reload request (the open session and the unsent draft). Hosts are
unchanged: bare `hya` and the WebUI host keep one pid per TUI, and the WebUI
host stays generic (ADR-0018). An in-process `execve` through `bun:ffi` was
rejected: it keeps every descriptor without `FD_CLOEXEC` (open backend
streams among them) and the blocked-signal mask, and depends on the libc of
each platform. Having the hosts restart the TUI on a special exit code was
rejected because it would put TUI knowledge into the generic web host and
leave a directly started TUI without the feature.

**What is reloaded.** The app is read from the TUI files on disk, not from
the generation pin: a checkout's `packages/hya-tui`, or the installed
`lib/hya/tui`. After a rollback the backend runs the pinned previous build
while the TUI runs the files on disk; the TUI's existing version check then
shows `backend <v> ≠ tui <v> · hya serve restart`. Only a `restart` triggers
a reload; crash recovery, `/reconnect`, and remote or fixed-URL TUIs do not.
The supervisor itself is loaded once per host start.
