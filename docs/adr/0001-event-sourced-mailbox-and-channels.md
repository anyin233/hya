# Event-sourced mailbox and channels

Inter-agent communication (direct mail by handle, named `#channels`, broadcast) is implemented as
first-class `Event`s (`MailSent`, `ChannelJoined`, `ChannelLeft`, `AgentRegistered`) appended to
the **team-root** session's log and folded by the shared `hya-proto::Projection` into per-agent
inboxes, channel logs, and a roster. We chose this over the pre-existing in-memory
`TeamControlPlane` (a `HashMap` state machine that was dead code) because the codebase mandates a
single event-sourced source of truth: making mail an event means the TUI channel/inbox view falls
out of the existing projection for free, mail survives process restart and replays
deterministically, resident-wake rides the existing `EventBus`, and there is no parallel read-model
to drift. The cost is new proto variants + reducer arms and a delivery/routing service on top; the
dead `TeamControlPlane` was deleted.

## Consequences

- The frontend renders from a *separate* projection (`hya-sdk::MessageStore`), so it had to grow a
  faithful `TeamProjection` mirror that folds the same events arriving over the `hya.envelope`
  global stream. That mirror must stay a pure read-model — no divergent logic.
- Address is a single type-safe `MailEndpoint` (`Handle | Channel`), not separate `to`/`channel`
  fields, so a message can never be ambiguously addressed to both.
