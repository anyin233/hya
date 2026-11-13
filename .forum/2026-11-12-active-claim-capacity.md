# What owns live-member capacity?

An active `resident_actor_claim`, indexed by root and state, is the capacity
lease. Registration and revival count and acquire claims in their writer
transactions; the terminal transaction releases them. A root turn finishing
or a process restarting cannot reset capacity. Workflow preflight reads the
remaining capacity and reserves nothing.

# Why is there no spawn-operation journal?

The production turn executor dispatches each collected tool call once. Its
round retry requires no text and no tool call; model fallback is pre-stream.
The shell path mints a fresh call id. Startup closes interrupted tool parts
rather than redispatching them. The old `Existing` consumer only rejected a
repeated request and never resumed it. Keep `ToolOperation` for provenance,
actor fencing and Workflow run identity; it provides no task replay API.

# Where must a task batch commit and become live?

Resolve first, then one `register_episodes` transaction registers all members
and lazy main or rolls everything back. Group-channel lookup shares that
transaction. Hold `claim_writes` and a `TeamRegistration` reservation from claim
commit through installation of every episode. Await hooks only after all are
installed. Each initializer publishes its committed envelopes after session
start hooks and before its own ending can publish archive envelopes.

Parent-path resolution must stay read-only: a root parent resolves to `main`
without invoking the mailbox's lazy-registration helper. Otherwise even the
existing-child registration path can leave main registered after a capacity
refusal, outside the registration transaction.
