# 0.45.9

## Subagent control API

- `ArchiveSubagent` (`POST /v1/sessions/{session}/archive`) archives a subagent session for the user, the same way its parent's `archive` tool does: its turn is cancelled (cause `archived`), live descendants archive first, and it leaves the live roster (`archived_by_parent`).
- `SteerSubagent` (`POST /v1/sessions/{session}/steer`, `{ text }`) sends the user's message to a subagent on its parent's DM, prefixed `[user] `. A running subagent sees it after its next tool call, an idle one wakes for it, and an archived one is revived by it.
- Both refuse a root session with `invalid_argument`. `CancelTurn` on a subagent's session interrupts only its turn; the subagent stays available.

## Runtime

- Fixed a redundant `TEAM QUIESCED` synthesis after the parent had already consumed subagent results through `wait` or `read channel://…`. Both delivery paths now acknowledge the resident supervisor after committing the inbox cursor, as automatic in-turn steering already does. Genuinely late unread mail still wakes the parent.
