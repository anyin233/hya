# Why is Idle not a mail-consumption boundary?

Idle is display activity, including a user-stopped turn. `ResidentWorkStarted`
records delivery and the outstanding admitted wake; `ResidentWorkFinished`
settles that work under its epoch and advances the cursor. In-turn
`MailConsumed` acknowledges mail but leaves the admitted wake outstanding, so a
later user cancel cannot lose the prompt even if a tool acknowledged its mail.
Both acknowledgements share the reducer's monotonic cursor rule.

A cancelled mail wake stays held with its work and is re-armed. Resume reuses
its existing user MessageId through begin_work's Admitted variant and appends
only newly hooked mail. A pre-admission cancellation admits nothing and re-arms
unread mail. A cancelled initial directive with no mail owes no autonomous wake;
it stays idle until mailed, as the existing interrupt contract requires.
Startup requeues Idle outstanding mail work. Busy crash work retains the prior
AbortedRunning policy and is settled under the recovered transaction.

# How is user cancellation distinguished from sidecar loss?

Bind cancellation to the turn lease before user-text hooks and race those hooks
with the effective token. At loss observation, sample the turn's existing cancel
cause and user hold before cancelling it for loss. A health failure that returns
Cancelled without a user/forced cause is still an interrupted sidecar turn. A
handle found dead after user cancellation is disposed without ending the held
episode. Forced ending ownership, reason and stop diagnostics remain with the episode task; a known ending retains a late dead handle for its own stop step.

# How were the synthesis-negative oracles checked?

The turn_end helper matches TEAM QUIESCED text regardless of message role.
Temporarily admitting a User directive before the count made both negative
assertions fail with 1 versus 0. The injection was removed; only the helper
correction remains.
