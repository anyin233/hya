# What closes a root turn stopped before its assistant message starts?

The engine checks the folded log for an unanswered user prompt and emits an
assistant start/finish pair with the actual ending cause. It does this even
when draining refuses the turn's first lease. No unanswered prompt means no
synthetic pair. The store row writer alone indexes shutdown finishes and
clears restores on assistant starts.

Daemon shutdown must also wait for the server run registry: engine leases do
not cover the interval between durable prompt admission and spawning the
engine task. `v1_restart` tests hold a real POST or startup restore in that
interval; engine-only tests miss it. See ADR-0031 for restore semantics.

A shell command stopped before `ToolCallRequested` still has no recorded
command text, so its restore prompt cannot name the command. This limitation
is recorded, not fixed, by the restart admission change.
