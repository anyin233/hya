# ADR-0031: Hot reload for extensions, stop/start for the binary, a restore prompt for interrupted sessions

Date: 2026-11-12
Status: Accepted

Supersedes [ADR-0028](0028-inherited-listener-handoff.md) entirely and the
restart/handover parts of [ADR-0023](0023-persistent-backend-daemon.md).

## Context

ADR-0028 kept the listening socket alive across a backend replacement: a
supervisor owned the listener, the new process inherited it, ran a self-check
against a `VACUUM INTO` copy, pinned a generation, and could be rolled back;
unanswered permission and question requests were handed to the new process
through a `pending_interaction_reply` table and `continue_after_handoff_*`
paths.

That machinery served one requirement, "a restart must not be observable", and
cost a second database-lock protocol, a parked-owner state, generation
pinning, a rollback launcher, and crash windows between its commits (audit
findings C7, C8, P9-F4, P9-F7). The two things that actually change at runtime
are much narrower:

- extensions (bundles, plugins, MCP servers, skills) change often and do not
  need a new process;
- the backend binary changes rarely, and a turn that is interrupted by it can
  be resumed by the model itself if it is told what happened.

## Decision

Three rules replace the handoff.

1. **An extension change is a hot reload.** Installing, removing, enabling, or
   editing a bundle, a plugin, or an MCP declaration republishes the affected
   source in the running daemon. The process and its PID do not change, and the
   next turn sees the new catalog. A source that fails to load keeps its last
   published state.
2. **A binary change is `stop` then `start`.** `hya serve stop` drains: it
   cancels every active turn and records one `pending_resume` row per
   interrupted session. `hya serve start` starts a fresh process that binds its
   own listener. There is no listener inheritance, self-check, generation pin,
   rollback launch, or parked owner. Clients reconnect, as they already do when
   a daemon goes away (ADR-0023).
3. **An interrupted session receives a restore prompt.** On start, each session
   with a `pending_resume` row gets one injected prompt stating that the
   previous turn was interrupted by a restart and which tool calls did not
   finish, and a new turn is started. The model decides what to redo.

Permission and question requests that were unanswered at `stop` are cancelled
with their turn. They are not carried across processes, and a tool call that
was waiting on one is reported as not finished; it is never recorded as
succeeded.

One database lock remains: the store's runtime-owner lock. A second `hya`
learns about the running daemon from the metadata file the owner writes, not
from probing the lock.

`hya update` installs the release and then applies rule 1 when only bundles
changed and rule 2 when the backend binary changed.

## Consequences

- A binary upgrade has a short window in which the port is closed. Frontends
  already handle a lost daemon by reconnecting.
- A turn interrupted by a restart is not continued mid-stream. Work done before
  the interruption stays in the event log; the model repeats unfinished tool
  calls if it judges that necessary, so tools with side effects may run twice.
- `--listen-fd`, `generation_pin`, `self_check`, the rollback launcher, the
  `pending_interaction_reply` table, and `continue_after_handoff_*` are deleted.
- The exit-75 reload supervision in the frontend launcher is reduced to
  reconnecting after a daemon restart.
