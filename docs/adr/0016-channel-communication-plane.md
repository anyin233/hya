# Channel communication plane (group broadcast + vertical DM)

ADR-0011 scoped a hierarchy mailbox by path arithmetic: an agent could address
its parent, its same-parent siblings, and its direct reports, with
unit-scoped named channels anyone could join. Under the ADR-0015 task model
there is nothing for those lateral edges to do — teams are strictly vertical
(up-report, down-dispatch) and the leader is the sole orchestrator of its
children. This ADR supersedes ADR-0011.

## Decision

Communication becomes a Discord/Slack-style channel plane with exactly two
channel kinds. Channel ids are minted as **event facts** (random 8-character
`[a-zA-Z0-9]` suffixes, collision-checked and re-minted within the team root);
replay stability comes from the log, not from derivation — the way session ids
already work. Randomness is a naming scheme, not a security boundary:
authorization is channel membership plus direction, enforced in the store
transaction that already validates liveness.

- **Group channel `#announce-{8}`, one per unit.** Members are the leader and
  its direct reports; **only the leader may post** (`broadcast`). The channel
  exposes **no member list** — it is a message pipe, not a directory. A child
  cannot tell how many siblings it has.
- **DM channel `#DM-{8}`, one per leader/report pair.** Created top-down at
  spawn, in the same transaction as registration, and **persistent across the
  child's archive/revive cycles** — it is the revival address. A DM to an
  archived direct child revives it (ADR-0015); nothing else can.
- **Vertical addressing only.** Siblings stay invisible: no member lists, no
  sibling DM. `dm` probes the acting identity from the session context, so a
  child never needs to know its own handle — its only DM peer is its parent; a
  leader addresses a child by handle learned from `task` or `search_agent`.
- **`list_channel`** lists only the caller's channels: home group channel,
  led group channel (when it leads), and DM channels of *live* peers with peer
  identity and unread counts. Archived peers' DM channels are not listed —
  archive discovery is `search_agent` over handoff digests (direct children
  only).
- **Broadcast never reaches archived members** (membership ends at archive);
  only a DM can wake one. Every new channel message wakes its recipients
  through the existing bus → wake path (sender excluded).
- All delivery rides `MailEndpoint::Channel`; a DM is simply a two-member
  channel. `roster`, `channels`, `join`, and `leave` are deleted; their
  information folds into `list_channel`. Named user-created channels do not
  exist.

## Consequences

- The write gate stops doing path arithmetic: it checks channel membership and
  role (leader-post group / participant DM / archived-peer-revive). Paths
  survive only for tree structure — handles, lineage, depth.
- ADR-0015's report gate yields a clean invariant here: while I am live, my
  parent is live, so an upward DM never resolves to an archived peer; revival
  is exclusively a downward act.
- Sibling coordination costs a relay through the leader (one extra turn each
  way). Accepted: it matches "the leader orchestrates its children" and depth
  is capped at two.
- Old logs with named channels and handle-addressed mail are not guaranteed to
  fold under the new rules. Accepted breakage under the redesign mandate; the
  fixtures that pin legacy folding are retired with the old model.
