# Why can a parent repeat its final answer after TEAM QUIESCED?

The resident supervisor tracks work epochs and pending wakes in memory in addition to the durable inbox cursor. Automatic in-turn steering both commits `MailConsumed` and calls `SessionEngine::notify_mail_consumed`. Explicit channel reads and channel-aware waits previously advanced only the durable cursor. Steering then saw no unread mail, skipped its own acknowledgement, and the supervisor could start a redundant synthesis after the parent's turn released its lease.

Both explicit delivery paths now notify the observer after a successful cursor commit. Regressions in `single_active_turn.rs` fail without each acknowledgement and pass with it. Mail arriving after a read must still wake the parent; do not suppress all quiescence follow-ups based only on whether an assistant message finished.

The reported session was titled “to-do app review with three subagent.” Its actual event log was unavailable in this workspace, so the reproduction confirms the backend defect, not the exact event ordering in that remote session.
