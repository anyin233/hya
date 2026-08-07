# Event-sourced mailbox and channels

> **Superseded in part by [ADR-0011](0011-hierarchy-scoped-mailbox.md).** The
> event-sourced single-log design below still holds in full. What changed is
> *who may address whom*: mail is no longer team-wide. An agent may address only
> its parent, its same-parent siblings, and its direct reports; handles are
> canonical paths (`main/lead-1/worker-2`); and channels are keyed by owning unit
> (`main/lead-1#build`). The Delivery rules below gain a scope gate that runs
> **before** the eligibility checks — see the note at the end of each section.

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
4. **Resolve the address in scope** (ADR-0011) — a relative leaf or a full path,
   matched only against agents the sender may address. This runs **before** every
   check below, so an out-of-scope address fails exactly like an unknown one and
   cannot be used to probe whether a teammate exists or is alive.
5. **Reject** with `StoreError::MailboxRejected` when:
   - the address is **not addressable from the sender** (out of scope, unknown,
     or ambiguous)
   - the target handle is **unknown** (not in the roster)
   - the target is a **transient non-root** member (`session != root` and `mode == Transient`)
   - the target is a **non-root** resident that fails eligibility
     (`session != root` and `mode == Resident` and not
     `resident_member_is_eligible` — stopped/terminal or no active claim)
6. Append `Event::MailSent { to: Handle(...) }` — carrying the **resolved
   canonical path**, not the string the sender typed
7. Commit and return the envelope

**Root-session handle:** when the roster entry's `session == root`, neither the
transient reject nor the resident eligibility check runs. Mail to the team-root
handle is accepted regardless of that entry's `RosterStatus` or claim row.

### `append_channel_mail`

Same writer-lock discipline and optional claim fence, then:

1. Replay the root projection
1. **Resolve the channel to a unit-qualified key** (ADR-0011): `#name` addresses
   the unit the sender leads if it leads one, otherwise its home unit; `#^name`
   addresses its home unit and is an error for an agent that leads nobody.
   `#announce` is reserved and refused here — see ADR-0011's announce path.
2. **Count** eligible subscribers on the channel (does not reject the send when zero are
   eligible — the `MailSent` is still appended)
3. Append `Event::MailSent { to: Channel(...) }`
4. Commit and return `(envelope, recipient_count)` where the count is the eligible set at
   send time

### Eligibility (`resident_member_is_eligible`)

Used for **non-root** resident direct-mail rejection and for channel recipient counting:

| Member | Counts as eligible? |
| --- | --- |
| Not on the roster / not resident | Yes for channel counting (non-resident always counts); direct mail still requires a roster entry |
| Resident with `RosterStatus::Done` or `Failed` | **No** (when this check is applied) |
| Resident otherwise | **Yes only if** `resident_actor_claim` has an **`active`** row for that actor session — the durable liveness check behind mail rejection |

Direct mail applies this only when `entry.session != root` and
`mode == Resident`. A roster entry whose session **is** the team root is never
passed through `resident_member_is_eligible` on the direct path.

The reducer's channel fan-out (above) uses the roster status skip only; the store's active-claim
check is the writer-side gate that prevents delivering *new* mail into a non-root resident that
no longer holds a live claim.
