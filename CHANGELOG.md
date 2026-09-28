# 0.43.14

## Fix: `hya serve restart` handoff works again

- `hya serve restart` failed on every daemon since the hot-reload merge: the successor stopped with `RUNTIME_OWNER_BUSY: runtime owner lock is already held`, and the old generation parked with its listener stopped. The merge dropped the engine's runtime-owner identity, so the old generation could not release its store claim for the successor. The engine again carries the owner that claimed the store; a regression test covers the release.
- `docs/cli.md` now documents `hya update apply --authorization FILE` instead of the removed `--owner-authorized-activation`.

## Daemon hot reload (merged from `feature/hot-reload-implementation`; its release notes were lost in the merge)

- `hya serve --listen-fd <FD>` adopts an already-open Unix TCP listener without rebinding the port (ADR-0028).
- `hya serve restart` transfers the listening socket and database lock to a successor generation running the binary the restart command was invoked with. Active root turns close at a durable handoff boundary and resume exactly once in the successor; completed tool calls are never replayed.
- Pending permission and question requests survive the handoff with stable ids; replies are durable and idempotent.
- Client streams receive `serverStopping {reason: "restart"}`, reconnect to the same URL, and re-read durable state.
- Updater activation requires an owner-issued capability bound to the candidate sequence and the active generation, under an updater-root lease with generation fencing.

```sh
cargo build -p hya-backend --bin hya
./target/debug/hya serve restart   # the running daemon hands off to this build
```
