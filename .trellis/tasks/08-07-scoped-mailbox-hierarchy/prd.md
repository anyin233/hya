# Scoped mailbox: limit agent comms to parent + siblings

## Goal

Replace the flat, team-wide mailbox with a **hierarchy-scoped** mailbox. An agent
may address only its parent, its same-parent siblings, and the agents it directly
leads. This keeps a large swarm tractable: the number of agents any one agent can
see stays bounded by its unit size, not by the swarm size.

## Problem

Today every agent in a team collapses to one **team root**
(`crates/hya-core/src/engine/mailbox.rs:28` walks the parent chain to the top).
All mail, all channels, and the whole roster live in one flat namespace under
that root. Consequences in a large swarm:

- `roster` returns every agent in the swarm. The list grows without bound.
- One channel namespace. Two unrelated sub-teams cannot both use `#build`.
- Any agent may direct-mail any other agent. Depth carries no meaning.

## Domain model

| Term | Definition |
| --- | --- |
| **Unit** | One leader plus the agents it directly leads. Written `unit(L)`. |
| **Leader** | An agent that has at least one child. It leads `unit(L)`. |
| **Home unit** | `unit(parent(X))` — the unit where `X` is a member with its peers. |
| **Led unit** | `unit(X)` — present only when `X` has children. |
| **Path** | An agent's canonical address: `main/lead-1/worker-2`. |
| **Leaf** | The last path segment: `worker-2`. |
| **Scope of X** | `{parent(X)} ∪ siblings(X) ∪ children(X)`. |

An agent with children belongs to **two** units: a member of its home unit, and
the leader of its led unit. Every other agent belongs to exactly one.

## Requirements

### R1 — Hierarchical path handles

- A handle is a `/`-separated path from the team root: `main/lead-1/worker-2`.
- The team root's path is the single segment `main`.
- Addressing accepts a **relative leaf** (`worker-1`) or the **full path**
  (`main/lead-1/worker-1`). Both must resolve to the same agent.
- A relative leaf resolves against the sender's scope only.

### R2 — Leaf uniqueness

- A leaf name must be unique among its siblings.
- A leaf name must differ from its parent's leaf.
- A registration that breaks either rule is **rejected at spawn time**, loudly.
- Consequence: within any scope, a relative leaf is never ambiguous.

### R3 — Direct-mail scope gate

- A send from `X` to `Y` is accepted only when `Y ∈ scope(X)`.
- An out-of-scope target is rejected with the same error class as an unknown
  target today (`StoreError::MailboxRejected`) and **no event is appended**.
- Existing rejects (unknown handle, transient non-root member, dead resident)
  keep their current behavior and are checked alongside the scope gate.

### R4 — Unit-scoped channels

- A channel belongs to exactly one unit. Its identity is `(unit, name)`, not
  `name`. Two units may each own a `#build`; they are different channels.
- `#name` addresses a channel in the sender's **led unit** when the sender is a
  leader, otherwise in its **home unit**.
- `#^name` addresses a channel in the sender's **home unit**. It is available
  only to a leader — a non-leader has no ambiguity to resolve and using `^` is an
  error.
- Join, leave, post, and list are all refused outside the sender's own two units.

### R5 — No cross-unit reach

- There is no grant, liaison, or skip-level path. The only route between units is
  a relay: `X → parent(X) → … → common ancestor → … → Y`. Each hop is an ordinary
  in-scope send.

### R6 — Announce is one-way, one level

- `announce` delivers to the sender's **direct children only**. It does not
  descend further.
- Reaching a whole subtree costs one announce per level; each hop is a deliberate
  act by that level's leader.
- Announce is not replyable on the announce path. A subordinate replies with
  ordinary direct mail to its parent, which R3 already permits.
- Announce reuses the existing `MailKind::Announcement` — no new event variant.

### R7 — Roster grouped by relation

`roster` returns the sender's `self` path plus three labeled groups:

```
self:    main/lead-1
parent:  main
peers:   main/lead-2
reports: main/lead-1/worker-1, main/lead-1/worker-3
```

A group that is empty is omitted. `channels` is scoped the same way and labels
each channel with the unit that owns it.

### R8 — Legacy logs replay with unchanged behavior

- `Event::AgentRegistered` gains `parent: Option<String>` as
  `#[serde(default)]`. A `None` parent means the team root.
- Every pre-existing event log therefore replays as **one flat unit under
  `main`**: the same agents, the same inbox contents in the same order, and every
  agent still able to address every other. Map **keys** are re-canonicalized
  (`reviewer-1` → `main/reviewer-1`, channel `build` → `main#build`); delivery
  behavior is unchanged. See the re-keying table in `design.md`.
- No config flag. The new behavior is the only behavior for new swarms.

## Constraints

- **ADR-0001 holds.** All team comms events stay in the single team-root log, and
  one replay must still reconstruct the whole team. No per-unit log sharding.
- **The reducer stays a pure fold.** It may not call `session_lineage` or touch
  the store. Everything scope resolution needs must be present in the events.
- **`hya-tool` may not depend on `hya-core`.** Scope resolution that needs the
  projection happens on the service side, reached over `MailboxPlane`.
- **The write gate stays `append_direct_mail` / `append_channel_mail`**, under
  the SQLite writer lock, so scope is checked in the same transaction that
  validates liveness.
- **`hya-sdk::TeamProjection` (`crates/hya-sdk/src/team.rs`) is a mirror.** It
  must fold the same events to the same result and stay free of divergent logic.
- Handle assignment must stay **deterministic** — no `rand`, no wall-clock — or
  replay stability breaks (`subagent.rs:25`).

## Acceptance criteria

- [ ] **AC1** A worker addressing a sibling by relative leaf succeeds; the same
      worker addressing an agent in another unit is rejected, and a replay of the
      root log shows **no** `MailSent` for the rejected send.
- [ ] **AC2** Full-path and relative-leaf addressing of the same in-scope agent
      produce identical delivery.
- [ ] **AC3** Registering a sibling whose leaf duplicates an existing sibling, or
      equals the parent's leaf, fails at spawn with a typed error.
- [ ] **AC4** Two units each own a `#build`. A post in one unit lands in that
      unit's subscribers only, and the other `#build` log is untouched.
- [ ] **AC5** A leader's `join("#build")` targets its led unit and
      `join("#^build")` targets its home unit; a non-leader using `^` is an error.
- [ ] **AC6** `announce` from a leader reaches its direct children and **not**
      its grandchildren. A grandchild receives it only after the intermediate
      leader announces in turn.
- [ ] **AC7** `roster` returns `self`/`parent`/`peers`/`reports`, omits empty
      groups, and never contains an out-of-scope agent.
- [ ] **AC8** An event log recorded before this change replays to the same team
      topology and the same delivery outcomes as today (same roster members, same
      inbox contents in the same order, one flat unit in which every agent may
      address every other), under canonically re-keyed maps.
- [ ] **AC9** A cross-unit message delivered by relay through the common ancestor
      arrives, proving R5 leaves a working path.
- [ ] **AC10** `hya-sdk::TeamProjection` folded over the same envelope stream
      equals the `hya-proto` projection, including scope and unit-qualified
      channel keys.
- [ ] **AC11** Handle assignment for a spawn batch is deterministic: the same
      roster plus the same batch order yields the same paths on repeat runs.

## Out of scope

- Any cross-unit grant, liaison, or skip-level mechanism (explicitly closed by R5).
- Subtree-wide broadcast (explicitly closed by R6).
- A config flag to restore the flat mailbox (explicitly closed by R8).
- Changing resident lifecycle, actor claims, or admission control. This task
  changes **who may talk to whom**, not how agents are scheduled.
