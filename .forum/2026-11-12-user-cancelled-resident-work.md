# Why is Idle not a mail-consumption boundary?

Idle is display activity, including a user-stopped turn. `ResidentWorkStarted`
records delivery and the outstanding admitted wake; `ResidentWorkFinished`
settles that work under its epoch and advances the cursor. In-turn
`MailConsumed` acknowledges mail but leaves the admitted wake outstanding, so a
later user cancel cannot lose the prompt even if a tool acknowledged its mail.
Both acknowledgements share the reducer's monotonic cursor rule.

A cancelled wake stays held with its work and is re-armed, even when its initial
directive had no mail. Resume reuses its existing user MessageId only if no
nonempty answer followed it. Otherwise begin_work settles the old boundary and
admits new mail (or a minimal continuation) at the tail in the same transaction.
The answered prompt remains immutable and mail follows the output it arrived
after. A pre-admission cancellation admits nothing and re-arms unread mail.
Historical inbox length alone never establishes owed work.
Startup requeues all Idle outstanding work. Busy crash work retains the prior
AbortedRunning policy and is settled under the recovered transaction.
A failed completion write ends the episode as Failed; an in-memory idle slot
does not establish that its work boundary committed.

# Which cancellation API installs the user hold?

`cancel_user_turn` holds the supervised subtree and cancels its active turns.
`cancel_turn` only cancels one lease and records its cause; passing `UserCancel`
does not install a hold. User-interrupt fixtures must use `cancel_user_turn`,
as the server does, to assert that unfinished work waits for explicit resume.

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
