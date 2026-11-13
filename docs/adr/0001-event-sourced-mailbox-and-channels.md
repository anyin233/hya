# Event-sourced mailbox and channels

> **Superseded in part by [ADR-0016](0016-channel-communication-plane.md).** The
> event-sourced single-log design below still holds in full. What changed is
> *who may address whom* and how channels are minted: addressing is strictly
> vertical (parent and direct reports, no sibling access); handles are canonical
> paths (`main/hya-plan-amiya/hya-task-texas`); and channels are event-minted
> unit broadcast (`#announce-{8}`) and pair DM (`#DM-{8}`) channels. See
> [ADR-0016](0016-channel-communication-plane.md) and
> [Subagent Orchestration](../architecture/subagent-orchestration.md) for the
> current channel contract.

Inter-agent communication (DM and unit broadcast) is implemented as
first-class `Event`s (`MailSent`, `ChannelCreated`, `AgentRegistered`) appended to
the **team-root** session's log and folded by the shared `hya-proto::Projection` into per-agent
inboxes, channel logs, and a roster. We chose this over the pre-existing in-memory
`TeamControlPlane` (a `HashMap` state machine that was dead code) because the codebase mandates a
single event-sourced source of truth: making mail an event means the TUI channel/inbox view falls
out of the existing projection for free, mail survives process restart and replays
deterministically, resident-wake rides the existing `EventBus`, and there is no parallel read-model
to drift. The cost is new proto variants + reducer arms and a delivery/routing service on top; the
dead `TeamControlPlane` was deleted.

## Consequences

- Frontends consume the shared server projection; they do not maintain a competing mail reducer.
- Address is a single type-safe `MailEndpoint` (`Handle | Channel`), not separate `to`/`channel`
  fields, so a message can never be ambiguously addressed to both.
- Channel fan-out reaches every current **eligible** subscriber, not literally every subscriber.
  When the reducer folds a `MailSent` addressed to a channel, it still appends the message to the
  channel log, then walks the current member set and **skips** any subscriber whose
  `RosterEntry.mode` is `resident` **and** whose `RosterStatus` is `Done` or `Failed`. A stopped
  actor's inbox therefore stops growing. The lead and non-terminal residents still receive
  the fan-out. (Writer-side eligibility for *new* mail is stricter — see Delivery rules.)

## Delivery rules

Handles are request aliases for existing sender–peer DM channels. The engine
resolves canonical paths, relative leaves and `^parent`/`^` at its boundary;
a missing DM rejects the send. There is no named/`^name` channel resolution.
For example, `send {"channel":"main/worker","body":"Continue"}` persists the
same target as an explicit `#DM-…` send. See [Agent channels](../agent-channels.md#handle-aliases-and-durable-mail)
for request, receipt and event schemas.

Mail is written under the SQLite writer lock by `append_channel_mail`:

1. `BEGIN IMMEDIATE`, optional sender `ActorClaim` fence, replay the root projection.
2. Require an existing channel, sender membership and direction allowed by the
   captured send policy. Groups require the unit leader.
3. Count subscribers with the shared replay eligibility predicate and query their
   resident claims in one batch. A live recipient without an active claim rejects
   the append. An archived direct-child DM returns `MailRevivalRequired` to the
   engine instead of appending. Revival rechecks this same gate in its transaction.
4. Append `MailSent { to: Channel(id), ... }`, commit, return its envelope and count.
   Callers publish only after commit. Outgoing copies never count as unread work.

`RosterEntry::accepts_mail` supplies the shared status rule: terminal residents
are ineligible, while the lead and non-terminal members receive channel copies.
Claims fence live-member admission. Archived members retain their parent DM;
only the parent can revive them, and unit broadcasts do not revive them.

Harness notices bypass agent send policy and use `MailSent { from: "harness",
to: Handle(path), ... }`. The common store event append rejects every other
handle-targeted event. `append_harness_mail` additionally requires a live
non-root resident recipient. `Handle` remains on the wire for this purpose;
`ChannelJoined` remains a wire variant but has no producer or reducer fold.
