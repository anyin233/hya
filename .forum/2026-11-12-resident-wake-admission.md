# What makes a resident wake atomic?

`SessionStore::begin_work` fences the episode and commits the coalesced prompt,
`ResidentWorkStarted` and Busy activity under one `BEGIN IMMEDIATE`. Main has no
claim and acknowledges its mail with `MailConsumed` in that transaction. The
returned message id is passed as `admitted_prompt` even for main synthesis.
Shutdown during binding then closes that answer and records the root's resume.
The row writer alone maintains `pending_resume`.

User-text hooks run before this transaction on the whole mail/synthesis text;
no plugin, provider or lock await runs inside it. The initial directive still
belongs to registration until M4-8: `ResidentWorkPrompt::Admitted` reuses its id
and appends any queued mail via `TextReplace` inside the wake transaction.

# How is a policy-denied mail batch consumed?

The listener checks every unread message from the larger of the durable and
in-memory cursors. If none is deliverable and no wake is pending, it consumes
the whole batch without a turn or turn-budget charge. Checking only whether
inbox length was cursor + 1 stranded two messages committed before either bus
envelope was handled. The deterministic regression publishes both committed
envelopes together at the production listener entry.
