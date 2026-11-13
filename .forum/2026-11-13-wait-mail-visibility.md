# Why must a wait/mail test hold the caller's turn lease?

The model's `wait` tool runs inside the caller's active turn. A test that registers main
as a resident but calls `wait` without holding main's turn lease lets the
supervisor start a mail turn first. `SessionStore::begin_work` then commits the
mail-injected user prompt and `MailConsumed` together. The later `wait` correctly
sees no unread mail, even when the sender's `mail_send` already returned.

At `1a20b439`, six concurrent loops of the complete wait test binary reproduced
this in 2 of 360 runs. A natural failure trace showed `MailSent` at sequence 21,
the root prompt containing that mail at 24, and `MailConsumed through=2` at 27;
wait scanned both inbox entries with cursor 2. Waiting on that consumption event
before the snapshot also forced the original assertion to fail without a sleep.

`timeout_zero_returns_the_current_state_at_once` now holds the existing
`hold_lead_turn` lease before spawning or sending, matching the tool's real
calling context. Do not change projection freshness or redeliver consumed mail
to compensate for an unleased test caller.
