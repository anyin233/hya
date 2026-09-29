# 0.43.23

## Fixes

- **Daemon startup survives the runtime-owner handover.** `hya serve start` now retries when the previous server has released its database lock but still holds the store's runtime-owner lock during shutdown. The temporary daemon exits with the existing retry code 75, and the starter continues until a healthy server appears or its start deadline expires.
- **Consecutive daemon restarts complete from one agent turn.** A restart requested by a resumed shell turn now waits for the previous handoff to reach `ready` and for its predecessor to exit before replacing the handoff journal. The old server also quiesces turns before it acknowledges `queued`, so a tool result cannot begin another model round ahead of the checkpoint.

The previous 0.43.22 notes are archived in `docs/changes/CHANGELOG_0.43.22.md`.
