# ADR-0031: Hot reload for extensions, stop/start for the binary, a restore prompt for interrupted sessions

Date: 2026-11-12
Status: Accepted

Supersedes the earlier supervisor-owned listener FD handoff (`ADR-0028`, removed)
entirely and the restart/handover parts of
[ADR-0023](0023-persistent-backend-daemon.md).

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
   refuses new server runs, cancels every active turn, and waits for the
   server run registry to empty, within the same drain deadline. A run
   reserved before the stop began admits nothing once it has begun. The
   store's event writer atomically indexes each
   assistant `MessageFinished { Cancelled, Shutdown }` in `pending_resume` for
   a non-archived root session, even when shutdown interrupted turn preparation
   before its assistant message started. Any assistant start deletes that row.
   Child sessions remain under the resident supervisor's recovery.
   `hya serve start` starts a fresh process that binds its
   own listener. There is no listener inheritance, self-check, generation pin,
   rollback launch, or parked owner. Clients reconnect, as they already do when
   a daemon goes away (ADR-0023).
3. **An interrupted session receives a restore prompt.** On start, each session
   with a `pending_resume` row gets one injected prompt stating that the
   previous turn was interrupted by a restart and which tool calls did not
   complete successfully, derived from the interrupted message's event log at
   startup. A new turn is started through normal server admission, including
   guidance, external directories, busy tracking, and cancellation. The row
   remains until the assistant turn starts, so a crash after prompt injection
   starts the turn on the next launch without injecting a second prompt for
   that interruption. A second graceful stop is a new interruption and receives
   its own prompt. An ordinary assistant turn also clears a pending restore.
   Missing, child, or archived sessions, rows whose interrupted message was
   removed by revert, and rows whose injected restore prompt was reverted
   (`/undo`) lose their rows; a session's restore failure
   is logged with its id and does not block other sessions or server startup.
   The model decides what to redo.

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

## Cancellation results

User cancellation, shutdown, and replacement stop collection at a cancellation
boundary. Before closing the assistant message, the engine persists the text
and reasoning already streamed and records captured file changes from tools
that already ran. A partial answer is therefore retained in replay. Use the
normal turn cancellation operation (or `hya serve stop` for shutdown); no
additional configuration is needed.

Resident and transient subagent activations follow the same rule: sidecar loss
cancels the turn token and awaits its cancelled result, including flushing and
message closure, before recovery or sidecar termination. The caller never drops
the running turn future to stop it.

Replay preserves the content a live subscriber saw, except for content an actor
produced after losing its claim. Every stale-claim write, including a flush of
text, reasoning, or file changes, is fenced; recovery closes the open message.
The claim fence takes precedence over replay/live equivalence. On cancellation,
live subscribers can see the durable `TextStart`/`TextEnd` again after their
live copies; the projection applies these boundaries idempotently. A straggler
resuming after the drain deadline may append a `ToolResult` after the drain's
`MessageFinished`, but cannot append a second finish.

A tool result already produced is recorded as that result even if cancellation
arrives immediately afterwards; the turn then finishes as cancelled. Cancelled
results bypass tool after-hooks, including direct shell calls, so a hook cannot
turn shutdown into success. File-change snapshots start after authorization:
an edit the user makes while permission is pending belongs to the user, and a
cancelled permission prompt records no file change for its unexecuted tool.

A prompt is owned by the turn that admitted it, and that turn closes it with
an assistant `MessageStarted` and `MessageFinished` pair however it ends:
cancellation or failure during preparation, refusal by a draining engine, or a
drain deadline that closes the turn on its behalf. Ownership is explicit: the
turn request carries the id of the prompt it admitted
(`run_turn_with_external_dirs_and_guidance(.., admitted_prompt)`), and only
such a turn emits a pair for a message that never started. A turn that admitted
nothing (a second caller queued behind the owner, an internal turn, a turn
after a background message) never answers a prompt, so one prompt never gets
two answers. A turn the user cancels before its assistant starts therefore
leaves an empty cancelled assistant message in the transcript, with cause
`UserCancel`, and clears any pending restore. For example, cancelling a
restored turn while its sidecar is starting prevents the next
`hya serve start` from restoring it again.

A resident mail wake of the root lead also carries the id of the user-role
prompt it admits. Stopping it during binding therefore creates the same
shutdown pair and restore row. A synthesis-only wake also admits and owns one
user prompt, so stopping it during binding records the same shutdown pair and
restore row. Child sessions still receive no restore row.

The server prepares a turn first (agent, guidance, external directories,
attachment checks) and commits nothing until preparation succeeded; a failed
preparation admits no prompt. It then reserves the session's run and hands it
to a detached task that admits the prompt (or injects the restore prompt) and
runs the turn that owns it. The HTTP handler only waits for the admission, so a
client that disconnects cannot leave a committed prompt without its owner.

`AppState::stop_for_restart(deadline: Duration) -> Result<TurnDrainReport,
CoreError>` refuses new server runs, drains engine turns, and waits for server
runs against one shared deadline. The existing HTTP `POST /v1/sessions/{id}/turns/{turn}/cancel` operation
and `MessageFinished { role: Assistant, finish, cause, tokens }` event contract
are unchanged. A shutdown finish retains cause `Shutdown`; a preparation error
closes with finish `Error` and its `Error { failed_message, code, message }` event.

The restore prompt lists children whose latest assistant message ended with
`Cancelled`/`Shutdown`, even if the child stopped before the root's assistant
message started. A later assistant answer removes that child from the list.

Stream collection returns `FinishReason::Cancelled`; tool collection returns
`ToolError::Cancelled`. Completed, failed, and cancelled turns use the same
message-closing implementation. The existing `MessageFinished` finish/cause
contract and shutdown restore indexing remain unchanged. Cancellation does not
undo disk writes; their `FilesChanged` events remain available for session revert.
