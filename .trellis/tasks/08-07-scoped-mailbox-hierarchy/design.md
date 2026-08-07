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

### `MailEndpoint` gains `Unit`

```rust
pub enum MailEndpoint {
    Handle(String),   // canonical path
    Channel(String),  // qualified key, see below
    Unit(String),     // NEW: the direct children of this leader path
}
```

`Unit` is the announce address (R6). It is a third address form rather than a new
`Event` variant, so `MailSent` and its whole delivery path are reused unchanged.
The reducer fans a `Unit(L)` send out to every roster entry whose parent is `L` —
direct children only, never deeper.

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
| Roster | `hya-proto/src/projection.rs:203` | `RosterEntry.handle` becomes the canonical path; add `parent: Option<String>` |
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

| Input | Behavior |
| --- | --- |
| Legacy log, no `parent` anywhere | Every member becomes a child of `main`. One flat unit. Every agent is every other agent's sibling → **today's behavior exactly**. |
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
