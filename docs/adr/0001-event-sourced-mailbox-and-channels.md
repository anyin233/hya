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
- Channel fan-out reaches every current **eligible** subscriber, not literally every subscriber.
  When the reducer folds a `MailSent` addressed to a channel, it still appends the message to the
  channel log, then walks the current member set and **skips** any subscriber whose
  `RosterEntry.mode` is `resident` **and** whose `RosterStatus` is `Done` or `Failed`. A stopped
  actor's inbox therefore stops growing. Transient members and non-terminal residents still receive
  the fan-out. (Writer-side eligibility for *new* mail is stricter — see Delivery rules.)

## Delivery rules

Mail is written under the SQLite writer lock in `hya-store` (`append_direct_mail` /
`append_channel_mail`). Callers publish the returned `Envelope` only **after** commit.

### `append_direct_mail`

1. `BEGIN IMMEDIATE`
2. Optional `ActorClaim` fence when the sender is a resident
3. Replay the team-root projection inside the transaction
4. **Reject** with `StoreError::MailboxRejected` when:
   - the target handle is **unknown** (not in the roster)
   - the target is a **transient non-root** member (`session != root` and `mode == Transient`)
   - the target is a **stopped/terminal resident** (see eligibility below)
5. Append `Event::MailSent { to: Handle(...) }`
6. Commit and return the envelope

### `append_channel_mail`

Same writer-lock discipline and optional claim fence, then:

1. Replay the root projection
2. **Count** eligible subscribers on the channel (does not reject the send when zero are
   eligible — the `MailSent` is still appended)
3. Append `Event::MailSent { to: Channel(...) }`
4. Commit and return `(envelope, recipient_count)` where the count is the eligible set at
   send time

### Eligibility (`resident_member_is_eligible`)

Used by both direct rejection and channel recipient counting:

| Member | Counts as eligible? |
| --- | --- |
| Not on the roster / not resident | Yes for channel counting (non-resident always counts); direct mail still requires a roster entry |
| Resident with `RosterStatus::Done` or `Failed` | **No** |
| Resident otherwise | **Yes only if** `resident_actor_claim` has an **`active`** row for that actor session — the durable liveness check behind mail rejection |

The reducer's channel fan-out (above) uses the roster status skip only; the store's active-claim
check is the writer-side gate that prevents delivering *new* mail into a resident that no longer
holds a live claim.
