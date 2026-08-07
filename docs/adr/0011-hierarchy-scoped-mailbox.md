# Hierarchy-scoped mailbox

An agent may address only its **parent**, its **same-parent siblings**, and its
**direct reports**. Everything else is out of scope. We chose this over the flat
team mailbox of ADR-0001 because that mailbox collapsed every agent in a team
onto one team root: one roster listing the whole swarm, one channel namespace, and
any agent able to direct-mail any other. At swarm scale the roster grows without
bound, two unrelated sub-teams cannot both use `#build`, and depth carries no
meaning. Scoping bounds what any one agent can see by its unit size rather than by
the size of the swarm.

The cost is a relay: crossing units now takes one hop per level in each
direction, and an announcement stops at any leader that does not pass it on.

## What did not change

**The log stays global.** ADR-0001's core requirement holds: every comms event is
still appended to the single team-root log, and one replay still reconstructs the
whole team. There is no per-unit log sharding and no second read-model. Scope is
enforced in exactly two places:

1. a **write gate** in `hya-store` (`append_direct_mail` / `append_channel_mail`),
   inside the transaction that already validates liveness, and
2. a **read filter** in `hya-core` when a tool asks for roster or channels.

## Model

A **unit** is one leader plus the agents it directly leads. An agent's canonical
handle is its path from the team root:

```
main                     the team root
main/lead-1              a child of the root
main/lead-1/worker-2     a child of lead-1
```

An agent with children belongs to **two** units: a member of its parent's unit,
and the leader of its own. Every other agent belongs to exactly one.

The rule is pure path arithmetic (`hya_proto::scope`), which is what lets the
write gate, the read filter, and the reducer share one definition instead of each
re-deriving it:

```rust
fn in_scope(from: &str, to: &str) -> bool {
    if from == to { return false; }
    let p = parent_path(from);
    to == p || parent_path(to) == p || parent_path(to) == Some(from)
}
```

## Consequences

- **Addressing accepts a relative leaf or a full path.** `worker-1` and
  `main/lead-1/worker-1` resolve to the same agent. Resolution runs **once**, at
  send time in the store, and the canonical path is what gets written into
  `MailSent.to` — so the reducer folds an address that is already resolved, and
  the log records what was actually meant.
- **Unknown and out-of-scope are indistinguishable.** Both produce the same
  rejection, and the scope check runs *before* the liveness checks. Otherwise an
  agent could probe whether an out-of-scope teammate existed, or was still
  running, by reading which rejection came back.
- **Leaf names are unique per unit, and differ from the parent's leaf.** The
  handle minter guarantees this by construction rather than rejecting a spawn: it
  skips any ordinal that would collide. Ordinals therefore count **per unit**, so
  `main/lead-1/reviewer-1` and `main/lead-2/reviewer-1` both exist.
- **Channels are unit-scoped.** The projection keys them by
  `{unit}#{name}` (`main/lead-1#build`), so two units may each own a `#build` and
  they are different channels. A bare `#name` addresses the unit you lead if you
  lead one, otherwise your home unit; `#^name` reaches your home unit and is an
  error for an agent that leads nobody.
- **No cross-unit reach at all.** No grant, no liaison, no skip-level. The only
  route between units is a relay through the common ancestor, where each hop is
  an ordinary in-scope send.

## Announce

`announce` is one-way and one level deep: it reaches the sender's **direct
reports** and stops. Reaching a whole subtree costs one announce per level, each
a deliberate act by that level's leader.

It is implemented as a reserved `{unit}#announce` channel that every agent
auto-joins at registration, **not** as a new `MailEndpoint` variant. The variant
was the first design and it is not wire-safe: `MailEndpoint` is adjacently tagged,
and `Event`'s `#[serde(other)] Unknown` catches only unknown event *types* — a
`MailSent` carrying an unknown *endpoint* is a known type with an unparseable
field, so an older binary would hard-error on replay instead of degrading. With
the reserved channel, an older binary parses the log and even delivers the
announcement correctly, because the `ChannelJoined` events are in the log too.

Only the unit's leader may post to it, which is what makes it one-way; replies
are ordinary direct mail to the parent, which the scope rule already permits. It
is hidden from channel listings and cannot be joined or left explicitly.

## Compatibility

`Event::AgentRegistered` gained `parent: Option<String>`, both
`#[serde(default)]` and `skip_serializing_if`, so a root registration is
byte-identical on the wire and a pre-scoping log — which has no `parent`
anywhere — folds as **one flat unit under `main`**: the same agents, the same
inbox contents in the same order, and every agent still able to address every
other. Map keys are re-canonicalized (`reviewer-1` → `main/reviewer-1`, channel
`build` → `main#build`); delivery behavior is unchanged.

`crates/hya-proto/tests/legacy_flat_mailbox.rs` pins this against a fixture
captured from the pre-scoping build. That fixture is the oracle and must never be
edited to accommodate a code change.

`hya_sdk::TeamProjection` mirrors the reducer for the TUI and does not depend on
`hya-proto` at runtime. `crates/hya-sdk/tests/team_mirror_conformance.rs` folds
one event stream through both and asserts they agree, which is what keeps a
hand-written mirror from drifting.
