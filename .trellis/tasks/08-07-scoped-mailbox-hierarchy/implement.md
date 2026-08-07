# Implementation plan — Scoped mailbox

Branch: `worktree-scoped-mailbox-hierarchy`
Worktree: `.claude/worktrees/scoped-mailbox-hierarchy`

## Ordering principle

Wire and helper changes land first and are **inert** — nothing enforces scope
until Phase 4. No intermediate commit ships a half-scoped mailbox, so every
commit boundary is a safe rollback point.

Each phase is TDD: write the failing test, implement, pass, then run the phase's
validation before moving on.

## Validation commands

Per-phase (fast):

```bash
cargo test -p <crate>
cargo clippy -p <crate> --all-targets -- -D warnings
```

Full gate (Phase 4 onward, and before the final commit):

```bash
cargo fmt --all --check
cargo clippy -p hya-proto -p hya-core -p hya-store -p hya-tool -p hya-app \
  --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```

### Pre-existing gate failures (NOT caused by this task)

`cargo clippy --workspace --all-targets -- -D warnings` and
`cargo fmt --all --check` **already fail on `main`**, entirely inside
`crates/hya-sdk`: 48 clippy errors (`unwrap`/`expect` in
`reducer.rs`, `server.rs`, `store.rs`, `team.rs`, `types.rs`,
`examples/native_spike.rs`) and 2 fmt diffs. Verified by checking that crate out
at `main` and re-running both commands.

Consequences for this task:

- The clippy gate is **scoped to the crates this task touches**, as above.
  Absorbing 48 unrelated fixes would bury the change under noise it did not
  cause.
- `cargo fmt --all` must **not** be run, because it rewrites `hya-sdk` files this
  task does not otherwise touch. Use `cargo fmt -p <crate>` per crate instead.
- Phase 8 edits `hya-sdk/src/team.rs`. Only that file is formatted and linted;
  the crate's other pre-existing failures stay as found and are reported, not
  silently fixed.

---

## Phase 0 — Legacy fixture (do this first)

Record a legacy event log **before** any code changes, so the back-compat oracle
is captured from the current build rather than reconstructed from a changed one.

- [ ] Capture an event log from today's flat mailbox: root + 2 members, a direct
      send, a channel join, and a channel post.
- [ ] Check it in as a fixture, plus the `Projection` it folds to today.
- [ ] Add a test asserting the fixture folds to that stored projection. It must
      pass now, and still pass at the end (**AC8**).

**Route:** `plan-executor-bulk` — mechanical capture, no design judgment.
**Gate:** the fixture test passes against unmodified code.

---

## Phase 1 — Path primitives (`hya-proto`)

Pure functions, no callers yet.

- [ ] `parent_path`, `leaf`, `join_path`, `is_valid_leaf` (rejects `/` and `#`).
- [ ] `in_scope(from, to)` exactly as specified in `design.md`.
- [ ] Channel-key qualify/parse: `{unit}#{name}` round-trips.
- [ ] Unit tests including: root has no parent, self is never in scope, a
      grandchild is not in scope, a nephew is not in scope.

**Route:** `plan-executor-heavy` — `in_scope` is the single definition of the
whole feature's rule; a subtle error here is invisible and total.
**Gate:** `cargo test -p hya-proto`. No other crate touched.

---

## Phase 2 — Wire changes (`hya-proto`)

- [ ] `AgentRegistered` gains `parent: Option<String>`, with **both**
      `#[serde(default)]` and `skip_serializing_if = "Option::is_none"` so a
      root registration stays byte-identical on the wire.
- [ ] `RosterEntry.handle` documented as the canonical path. **No `parent`
      field** — the path already encodes it.
- [ ] `MailEndpoint` is **not** changed (see the correction in `design.md`).
- [ ] Serde round-trip tests, and a test that a payload **without** `parent`
      still deserializes.

Existing call sites must keep compiling — fill `parent: None` at every emit site
for now. Behavior is unchanged at the end of this phase, by construction.

**Route:** `plan-executor-heavy` — a breaking change to `Event`/`MailEndpoint`,
which is wire format.
**Gate:** full workspace builds; `cargo test --workspace --exclude hya-e2e` still
green with **no behavior change**.

---

## Phase 3 — Reducer (`hya-proto/src/projection.rs`)

- [ ] Derive the canonical path in the `AgentRegistered` arm, per the three-row
      table in `design.md` (root case, `parent = Some`, legacy `None`).
- [ ] Qualify channel keys in the `ChannelJoined` / `ChannelLeft` / `MailSent`
      channel arms.
- [ ] No new fan-out arm: announce rides the existing channel fan-out via the
      reserved `{unit}#announce` channel, whose membership is the unit's direct
      children (auto-joined in Phase 6).
- [ ] Legacy leaf fallback for bare handles in `MailSent.to`, `MailSent.from`,
      `AgentActivityChanged.handle`, `ResidentWorkStarted.handle`.
- [ ] Phase 0's fixture test must still pass (**AC8**).

**Route:** `plan-executor-heavy` — projection/reducer change; a wrong fold
silently corrupts every downstream read.
**Gate:** `cargo test -p hya-proto`; the Phase 0 fixture test is the acceptance
oracle.

---

## Phase 4 — Write gate (`hya-store/src/mailbox.rs`) ← behavior switches on here

- [ ] `append_direct_mail`: resolve `to` against the sender's scope, then
      `in_scope` check, **before** the existing unknown/transient/dead checks.
      Reject with `MailboxRejected` and append nothing (**AC1**).
- [ ] `append_channel_mail`: qualify the channel key from the sender's path and
      the `#` / `#^` form; refuse a channel outside the sender's two units; count
      eligible subscribers within that unit only.
- [ ] Tests: sibling accepted, cousin rejected, parent accepted, child accepted,
      grandparent rejected, relative leaf and full path agree (**AC2**), two
      units each own a `#build` and do not cross-talk (**AC4**).
- [ ] Test that the scope reject fires **before** the liveness check, so an
      out-of-scope send cannot probe whether a target is alive.

**Route:** `plan-executor-heavy` — the security boundary of the feature, inside a
writer transaction.
**Gate:** full gate. This is the first commit that changes observable behavior
and the primary rollback point.

---

## Phase 5 — Engine routing (`hya-core`)

- [ ] `team_roster` → scoped `ScopedRoster` (self / parent / peers / reports).
- [ ] `team_channels` → the sender's two units only, each labeled with its owner.
- [ ] `channel_join` / `channel_leave` qualify `#` and `#^`; a non-leader using
      `^` is an error (**AC5**).
- [ ] `announce`: emit `MailSent { to: Channel("{self}#announce"),
      kind: Announcement }`; reject from an agent with no children.
- [ ] `#announce` is reserved: `join` / `leave` / ordinary posts to it are
      refused, and it is hidden from the `channels` listing.
- [ ] `resolve_handle` returns the canonical path.
- [ ] Test: announce reaches direct children and **not** grandchildren (**AC6**).
- [ ] Test: cross-unit relay through the common ancestor arrives (**AC9**).

**Route:** `plan-executor-heavy` — routing plus the new announce primitive.
**Gate:** `cargo test -p hya-core`, then full gate.

---

## Phase 6 — Handle minting (`hya-core/src/subagent.rs`, `resident.rs`)

- [ ] `assign_handles`: count ordinals **per unit** (entries whose parent is the
      spawning lead's path), mint `{lead_path}/{type}-{ordinal}`.
- [ ] Bump the ordinal when the minted leaf equals the parent's leaf (R2).
- [ ] Reject a duplicate sibling leaf at registration with a typed error
      (**AC3**).
- [ ] Pass the real `parent` at every `AgentRegistered` emit site
      (`subagent.rs:261`, `resident.rs:1833`; root stays `None` at
      `engine/mailbox.rs:88`).
- [ ] Emit a `ChannelJoined` for `{parent}#announce` alongside every
      `AgentRegistered`, so the announce channel's membership is the unit's
      direct children and is replayable by older binaries too.
- [ ] `resident.rs:842` `recipient_sessions`: canonical-path lookups; keep
      self-wake exclusion (no new arm needed).
- [ ] Determinism test: same roster + same batch order → same paths (**AC11**).

**Route:** `plan-executor-heavy` — replay determinism; a nondeterministic handle
breaks every future replay silently.
**Gate:** `cargo test -p hya-core`, then full gate.

---

## Phase 7 — Tool surface (`hya-tool/src/mailbox.rs`)

- [ ] `MailboxRequest::Roster` reply type → `ScopedRoster`.
- [ ] `render_roster` → grouped output, empty groups omitted (**AC7**).
- [ ] `channels` labels each channel with its owning unit.
- [ ] New `announce` tool: `body` only, documented as one-way to direct reports.
- [ ] `send` schema text: relative leaf or full path, scope rule, `#` vs `#^`.

**Route:** `plan-executor-bulk` — schema and rendering, driven by settled
decisions. Escalate if the `ScopedRoster` plumbing turns out to need engine
changes.
**Gate:** `cargo test -p hya-tool`.

---

## Phase 8 — SDK mirror + TUI (`hya-sdk/src/team.rs`, TUI roster view)

- [ ] Mirror canonicalization, qualified channel keys, and the `Unit` fan-out in
      `apply_agent_registered` / `apply_channel_*` / `apply_mail_sent`.
- [ ] **Conformance test:** fold one envelope stream through both
      `hya_proto::Projection` and `hya_sdk::TeamProjection`; assert the team state
      matches (**AC10**). This is the guard against mirror drift.
- [ ] TUI: render path handles legibly (leaf prominent, path as context) and
      unit-qualified channel names.

**Route:** `plan-executor-bulk` for the mirror (rule-driven, mirrors a settled
reducer); `plan-executor-heavy` if the conformance test exposes a reducer defect.
**Gate:** full gate.

---

## Phase 9 — E2E + docs

- [ ] Extend `crates/hya-e2e/tests/p16_swarm_mailbox.rs` with a two-unit swarm:
      in-unit delivery works, cross-unit is rejected, relay arrives.
- [ ] Update `docs/adr/0001-event-sourced-mailbox-and-channels.md` — the delivery
      rules table now has a scope row; note that the log stays global.
- [ ] New ADR for the scope rule itself (units, paths, announce, no cross-unit).
- [ ] Update `docs/architecture/event-model.md` for the wire changes.

**Route:** `plan-executor-bulk`, then `doc-scribe` for the ADR prose.
**Gate:** full gate including `cargo test -p hya-e2e -- --test-threads=1`.

---

## Review gates

| After | Check |
| --- | --- |
| Phase 1 | `in_scope` truth table reviewed by hand against the PRD domain model |
| Phase 3 | Legacy fixture folds identically (**AC8**) — the back-compat proof |
| Phase 4 | Adversarial review of the write gate: can any input reach an out-of-scope inbox? |
| Phase 6 | Replay determinism argued, not just tested |
| Phase 8 | Reducer/mirror conformance green (**AC10**) |
| Phase 9 | Every AC1–AC11 mapped to a named test |

## Rollback points

- **Before Phase 4** — everything is additive and inert. Revert is free.
- **After Phase 4** — revert the gate commit to restore flat behavior. The
  additive wire fields stay; `#[serde(default)]` means logs written under the
  scoped build stay readable by the reverted build.
- **No data migration exists to undo.** Nothing on disk is rewritten.

## Open items to confirm during implementation

- Whether any consumer outside the listed files string-matches a roster handle.
  Grep before Phase 6; anything found is added to Phase 8.

## Decisions taken during implementation

- **Resolution lives on `TeamProjection`, not in `scope`.** `scope` stays pure
  path arithmetic; anything needing the roster (`resolve_in_scope`,
  `resolve_channel`, `scoped_roster`, `scoped_channels`, `leads_a_unit`) is a
  method on `TeamProjection`. Both the store gate and the engine reader call the
  same methods, so there is one definition of each rule.
- **Unknown and out-of-scope are indistinguishable.** `resolve_in_scope` returns
  `None` for both, and the store's rejection message covers both. Otherwise a
  sender could probe whether an out-of-scope agent existed by reading which
  rejection came back.
- **`parent_agent_path` falls back to the team root.** The public
  `register_existing_resident*` entry points take a handle but no parent, so the
  unit is re-derived from the session's own lineage. When the parent is unknown or
  itself unregistered, the agent is placed at the root — the pre-scoping flat
  arrangement — rather than failing the spawn and stranding it.
- **`spawn_resident` now returns a canonical path**, not a bare leaf, so callers
  that hold a handle (stop, activity records) carry the same identity the roster
  uses.

## Resolved during implementation

- **`MailEndpoint` variant compat** — RESOLVED before Phase 1. `MailEndpoint` is
  adjacently tagged and `Event`'s `#[serde(other)] Unknown` catches only unknown
  event *types*, so a new endpoint variant would hard-error an older binary.
  Announce now rides a reserved auto-joined `{unit}#announce` channel instead;
  `MailEndpoint` is unchanged. See the correction in `design.md`.
- **AC8 wording** — RESOLVED before Phase 0. A legacy log is re-keyed to
  canonical paths, so map equality was the wrong assertion. AC8 now asserts
  topology and delivery-outcome equivalence.
