# Design — Scoped mailbox

## Design principle

**The log stays global; only visibility becomes scoped.**

ADR-0001 requires that one replay of the team-root log reconstructs the whole
team. This design keeps that: every comms event still lands in the team-root log,
and the projection still holds one `TeamProjection`. Scope is enforced at two
points and nowhere else:

1. a **write gate** in `hya-store`, inside the existing writer transaction, and
2. a **read filter** in the `hya-core` engine, when a tool asks for roster or
   channels.

No per-unit log sharding, no second read-model, no config flag.

## Addressing

### Canonical path

An agent's canonical handle is its path from the team root:

```
main                       the team root
main/lead-1                a child of the root
main/lead-1/worker-2       a child of lead-1
```

`parent_path("main/lead-1/worker-2") == "main/lead-1"`.
`leaf("main/lead-1/worker-2") == "worker-2"`.

A leaf may not contain `/` or `#`. Both are structural separators, so the parse
of any stored address is unambiguous.

### Scope is pure string math

`scope(X) = {parent(X)} ∪ siblings(X) ∪ children(X)` needs no roster walk:

```rust
/// True when `from` may address `to` under the unit rule.
/// Pure path arithmetic — no store access, no roster walk, O(len).
fn in_scope(from: &str, to: &str) -> bool {
    if from == to { return false; }               // no self-mail (existing rule)
    let from_parent = parent_path(from);
    match () {
        _ if to == from_parent            => true, // my parent
        _ if parent_path(to) == from_parent => true, // my sibling
        _ if parent_path(to) == Some(from)  => true, // my direct report
        _ => false,
    }
}
```

This same predicate serves the write gate, the read filter, and the tests. It is
the single definition of the rule.

### Resolution happens at send time, not fold time

The model may type a relative leaf (`worker-1`) or a full path
(`main/lead-1/worker-1`). Resolution runs **once**, in `hya-store` inside the
validating transaction, and the **canonical path is what gets written into
`MailSent.to`**.

```
model types "worker-1"
  → store resolves against scope(sender) → "main/lead-1/worker-1"
  → in_scope check
  → Event::MailSent { to: Handle("main/lead-1/worker-1") }
```

Consequences: the reducer stays trivial (it folds an address that is already
canonical), and the log records what was actually meant, which is what an audit
of a swarm needs.

## Wire changes

All three are additive and `#[serde(default)]`, so an older binary folds a newer
log without panicking, and a newer binary reads an older log.

### `Event::AgentRegistered` gains `parent`

```rust
AgentRegistered {
    session: SessionId,          // team-root log (unchanged)
    agent_session: SessionId,    // unchanged
    handle: String,              // now the LEAF, not a team-wide name
    #[serde(default)]
    parent: Option<String>,      // NEW: parent's canonical path; None = team root
    #[serde(default)]
    agent_type: AgentName,
    #[serde(default)]
    mode: SubagentMode,
}
```

Canonical path is derived at fold time from the event's own fields — the reducer
never needs the store:

| Case | Canonical path |
| --- | --- |
| `agent_session == session` (root registration) | `"main"` |
| `parent = Some(p)` | `"{p}/{handle}"` |
| `parent = None`, non-root (**legacy only**) | `"main/{handle}"` |

The third row is what makes R8 work: a legacy log has no `parent` anywhere, so
every member becomes a direct child of `main` — one flat unit, which is exactly
today's behavior.

### `MailEndpoint` is unchanged — announce is a reserved auto-joined channel

**Correction to the first draft.** The draft added `MailEndpoint::Unit(String)`.
That is not wire-safe. `MailEndpoint` is adjacently tagged
(`#[serde(tag = "kind", content = "id")]`, `mail.rs:18`) and `Event`'s
`#[serde(other)] Unknown` (`event.rs:532`) only catches an unknown **event
type** — a `MailSent` carrying an unknown *endpoint* variant is a known type with
an unparseable field, so an older binary would hard-error on replay instead of
degrading. That breaks the project's stated additive-wire property.

Announce is therefore modelled with the existing `Channel` variant:

- Every unit has one reserved channel, qualified key `{unit}#announce`.
- Every agent **auto-joins its parent's announce channel** at registration, via a
  real `ChannelJoined` event emitted alongside `AgentRegistered`.
- `announce` is an ordinary `MailSent { to: Channel("{unit}#announce"),
  kind: Announcement }`.
- The write gate permits a post to `{unit}#announce` **only from that unit's
  leader**, which is what makes it one-way (R6).
- `#announce` is reserved: `join`, `leave`, and ordinary posts to it are refused
  through the channel tools, and it is hidden from the `channels` listing.

This is strictly better than a new variant:

| Property | `MailEndpoint::Unit` | Reserved auto-joined channel |
| --- | --- | --- |
| Old binary reads a new log | **hard deserialize error** | parses; folds correctly |
| Old binary delivers the announce | no | **yes** — the `ChannelJoined` events are in the log too |
| New reducer arm needed | yes | no — existing channel fan-out |
| `MailEndpoint` blast radius | every `match` in the workspace | none |

Auto-join reaches **direct children only**, so the one-level rule of R6 falls out
of the membership set rather than needing a special fan-out.

Cost: one extra `ChannelJoined` event per agent, and one reserved channel name.

### Channel keys become unit-qualified

`TeamProjection.channels` is keyed by `{owning_unit_leader_path}#{name}`:

```
main#build              the root unit's #build
main/lead-1#build       lead-1's unit's #build   — a DIFFERENT channel
```

A legacy `ChannelJoined { channel: "build" }` qualifies to `main#build`, because
a legacy log is one flat unit under `main`. Deterministic, no store access.

`#name` / `#^name` resolve to a qualified key at send/join time:

| Sender | `#build` resolves to | `#^build` resolves to |
| --- | --- | --- |
| leader `main/lead-1` | `main/lead-1#build` (led unit) | `main#build` (home unit) |
| leaf `main/lead-1/worker-2` | `main/lead-1#build` (home unit) | **error** — no led unit |

## Component changes

| Component | File | Change |
| --- | --- | --- |
| Event | `hya-proto/src/event.rs:435` | add `parent` to `AgentRegistered` |
| Address | `hya-proto/src/mail.rs:19` | add `Unit`; add path/leaf/qualify helpers; forbid `/` and `#` in leaves |
| Roster | `hya-proto/src/projection.rs:203` | `RosterEntry.handle` becomes the canonical path (no `parent` field — see below) |
| Reducer | `hya-proto/src/projection.rs:568` | derive canonical paths; qualify channel keys; add the `Unit` fan-out arm |
| Write gate | `hya-store/src/mailbox.rs:38,85` | resolve + `in_scope` check before the existing liveness checks |
| Routing | `hya-core/src/engine/mailbox.rs` | scope-filter roster/channels reads; qualify join/leave; add announce |
| Handles | `hya-core/src/subagent.rs:36` | per-unit ordinals; leaf-collision rules |
| Residents | `hya-core/src/resident.rs:842` | `Unit` arm in `recipient_sessions`; canonical-path lookups |
| Tools | `hya-tool/src/mailbox.rs` | grouped roster; unit-labeled channels; new `announce` tool |
| SDK mirror | `hya-sdk/src/team.rs:126` | mirror all of the above, still pure read-model |

### Write gate — `append_direct_mail`

Order matters. Scope is checked **before** liveness so an out-of-scope send never
leaks whether the target is alive:

```
BEGIN IMMEDIATE
  fence actor claim (unchanged)
  replay root projection (unchanged)
  resolve `to` against scope(sender)   ── NEW
  reject if not in_scope               ── NEW  → MailboxRejected, no append
  reject if unknown / transient / dead    (unchanged)
  append MailSent { to: canonical }
COMMIT
```

Every reject still happens **before** the append, so a rejected send leaves no
trace in the log — the property AC1 asserts and that the existing tests
(`direct_mail_to_transient_member_is_rejected_before_append`) already establish.

### Handle minting — `assign_handles`

Two changes, both required for determinism:

1. **Ordinals count per unit, not per team.** Only roster entries whose parent is
   the spawning lead's path are counted. So `main/lead-1/reviewer-1` and
   `main/lead-2/reviewer-1` both exist and neither is a collision.
2. **Parent-leaf collision bumps the ordinal.** A `lead`-type agent at
   `main/lead-1` spawning a `lead`-type child would mint leaf `lead-1`, which
   equals the parent's leaf and breaks R2. The minter detects this and takes the
   next ordinal (`lead-2`). Deterministic — derived only from the roster and the
   batch order, per the constraint at `subagent.rs:25`.

### Roster read filter

`MailboxRequest::Roster` returns a new grouped type instead of a flat
`Vec<RosterEntry>`:

```rust
pub struct ScopedRoster {
    pub self_path: String,
    pub parent: Option<RosterEntry>,
    pub peers: Vec<RosterEntry>,
    pub reports: Vec<RosterEntry>,
}
```

Built by filtering the full roster through `in_scope`, then bucketing by the path
relation. Empty groups are omitted at render time (R7).

## Back-compat matrix

**`RosterEntry` needs no `parent` field.** The canonical path already encodes it:
`parent_path("main/lead-1/worker-2") == "main/lead-1"`. Storing it separately
would be a second source of truth that can disagree with the key. The `parent`
field exists only on the **event**, where the reducer needs it to build the path.

**AC8 restated (correction).** The first draft claimed a legacy log folds to a
projection *equal* to today's. That is false in the literal sense: the roster,
inbox, and channel maps are **re-keyed** from bare names to canonical paths
(`reviewer-1` → `main/reviewer-1`, `build` → `main#build`). What is preserved is
the **topology and every delivery outcome** — the same agents, the same inbox
contents in the same order, one flat unit in which every agent may address every
other. AC8 asserts that, not map equality. The re-keying is:

| Legacy key | Canonical key |
| --- | --- |
| roster / inbox `main` | `main` |
| roster / inbox `reviewer-1` | `main/reviewer-1` |
| channel `build` | `main#build` |

| Input | Behavior |
| --- | --- |
| Legacy log, no `parent` anywhere | Every member becomes a child of `main`. One flat unit. Every agent is every other agent's sibling → **today's delivery behavior exactly**, under re-keyed map keys. |
| Legacy `MailSent { to: Handle("reviewer-1") }` | Folded via the leaf fallback: a bare leaf resolves against the roster, which is unique in a flat legacy team. |
| Legacy `ChannelJoined { channel: "build" }` | Qualifies to `main#build`. |
| Legacy `AgentActivityChanged { handle: "reviewer-1" }` | Same leaf fallback as mail. |
| New log read by an old binary | `parent` and `Unit` are unknown fields/variants; the existing `Unknown` fold path handles them. Scope is lost, which degrades to today's flat behavior — not a crash. |

The leaf fallback is **only** reachable for logs written before this change. New
emitters always write canonical paths, so the fallback branch is dead for new
logs. It must be tested explicitly against a recorded legacy log, or it will rot.

## Tradeoffs accepted

- **A relay costs latency.** Cross-unit traffic takes one hop per level in each
  direction. This is the point of R5, not a defect, but a deep swarm pays for it.
- **A lazy leader is a single point of failure for announcements.** R6 means an
  all-hands stops at any leader that does not relay it. Accepted to keep one
  uniform rule; the alternative was a privileged subtree broadcast.
- **`RosterEntry.handle` semantics change** from a team-unique name to a path.
  Every consumer that displays or matches a handle must be revisited. This is the
  largest blast radius in the change and the main reason for the phased plan.
- **`#^` is new syntax** the model must learn. It only appears for leaders, which
  are the minority of agents.

## Risks

| Risk | Mitigation |
| --- | --- |
| Handle-path change silently breaks a consumer that string-matched a handle | Grep every `handle` consumer; the TUI and SDK mirror are named explicitly in the plan |
| Reducer and SDK mirror drift on canonicalization | One shared conformance test folds the same envelope stream through both and asserts equality (AC10) |
| Legacy leaf fallback rots | A checked-in legacy log fixture replayed in CI (AC8) |
| Per-unit ordinals break replay determinism | Property test: same roster + same batch order → same paths (AC11) |
| Scope check placed after liveness leaks target existence | Gate order fixed above and asserted by a test |

## Rollout / rollback

- **Rollout:** one branch, phased commits (see `implement.md`). Wire changes land
  first and are inert until the gates switch on, so no intermediate commit ships
  a half-scoped mailbox.
- **Rollback:** `git revert` of the gate commit restores flat behavior while
  leaving the additive wire fields in place. Logs written under the scoped build
  stay readable, because `parent` is `#[serde(default)]` and the flat path
  ignores it.
- **No data migration.** Nothing is rewritten on disk. Legacy logs are
  reinterpreted at fold time, not converted.
