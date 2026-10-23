# 0.45.9

## Subagent control API

- `ArchiveSubagent` (`POST /v1/sessions/{session}/archive`) archives a subagent session for the user, the same way its parent's `archive` tool does: its turn is cancelled (cause `archived`), live descendants archive first, and it leaves the live roster (`archived_by_parent`).
- `SteerSubagent` (`POST /v1/sessions/{session}/steer`, `{ text }`) sends the user's message to a subagent on its parent's DM, prefixed `[user] `. A running subagent sees it after its next tool call, an idle one wakes for it, and an archived one is revived by it.
- Both refuse a root session with `invalid_argument`.

## User cancel stops the subtree

- `CancelTurn` on a session now also cancels the turns of every live subagent below it, and keeps them all stopped: no mail wake and no `TEAM QUIESCED` synthesis restarts them. Previously a cancel reached only that session, and the parent's `wait` reported the interrupted child as stalled, so the parent re-sent the work within seconds.
- A parent's mail to a stopped subagent is refused with a message telling it to ask the user. `wait` lists the subagent as `stopped_by_user` and does not wake for it.
- The user lifts the stop with a new prompt on the session (`CreateTurn`), or with `SteerSubagent` on the subagent. Nothing is archived, and the subagents keep their transcripts.

## Runtime

- Fixed a redundant `TEAM QUIESCED` synthesis after the parent had already consumed subagent results through `wait` or `read channel://…`. Both delivery paths now acknowledge the resident supervisor after committing the inbox cursor, as automatic in-turn steering already does. Genuinely late unread mail still wakes the parent.
