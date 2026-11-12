# What is the atomic boundary for a resident revival?

`SessionStore::revive_episode` rechecks the original handle/channel authorization
under `BEGIN IMMEDIATE`, activates the released claim, appends AgentRegistered
and AgentRestarted, reads the latest child handoff and writes its system context
boundary, then appends the reviving mail on the existing parent DM. It returns
the claim and old inbox length with committed envelopes. A concurrent sender
that finds a live recipient appends ordinary mail and receives no new claim.

The supervisor reserves the selected team and serializes commit/install with
all endings and Takeover decisions through `claim_writes`. It waits for a
previous episode's `done` outside that lock only while the durable member is
archived; it must never wait for another sender's newly live episode.
Initialization publishes the transaction envelopes before any terminal effects.

# What happens to mail from an ended episode?

New roster insertion starts its cursor at the current inbox length. Old handled
mail is skipped, and old unread mail stays unread in history, represented by
Pending in the degraded handoff (owner decision E2). The reviving MailSent comes
after registration, so begin_work admits only new owed work; completion consumes its boundary.
Archive removes broadcast membership; revived registration restores the existing
parent-unit channel membership without creating a replacement DM or channel.

# Why must lifecycle test scripts be bound to a recipient?

A DM's channel history is folded into both peers' inboxes. Even though the live
wake router never wakes the sender, a later main synthesis can include its own
outbound mail. A provider fixture that chooses scripts by marker text alone can
consume the child's script in the parent before a slower sidecar starts.
`Harness::mail` binds matching script markers to the target session. Assertions
in I9–I12 remain intact. Summarizer provider calls use synthetic session IDs, so
summary-input assertions select tool-less requests rather than the actor ID.

# How should lifecycle tests isolate a team budget?

Gate the child's initial provider call, acquire a real root turn lease before
releasing that gate, and retain it until the next child wake is blocked. Without
that lease, setup quiescence can start a main synthesis and spend a turn before
the operation under test. Reviving mail also counts as an actual DM for the
message budget; the revived User message appears later at `begin_work`, so
transcript checks must observe admission rather than assume `mail_send` admits.

The same lease is needed when a test counts provider polls for a child through
Idle or failure. Main's legitimate `TEAM QUIESCED` synthesis uses the shared
provider too. Under three concurrent stress loops, the declaration-drift test
counted that root poll as a forbidden second child poll; waiting for the real
root synthesis reproduced it deterministically. Keep the child-count assertion
and exclude root work with a real root turn lease.
