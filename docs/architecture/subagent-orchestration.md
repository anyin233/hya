# Subagent Orchestration

This document is the module-level specification for the unified subagent
orchestration redesign. It is normative for implementation; the decisions and
their rationale live in the ADRs:

- [ADR-0015](../adr/0015-unified-resident-subagent-lifecycle.md) — unified
  resident lifecycle (episodic actors), supersedes ADR-0002's hybrid model.
- [ADR-0016](../adr/0016-channel-communication-plane.md) — channel
  communication plane, supersedes ADR-0011.
- [ADR-0017](../adr/0017-workflow-on-unified-substrate.md) — workflow
  execution on the unified substrate.

The redesign is **breaking by mandate**: legacy logs and tool shapes are not
preserved. Compatibility shims are not part of any phase.

Related: [admission-and-governor.md](admission-and-governor.md),
[event-model.md](event-model.md), [agent-tool-surface.md](agent-tool-surface.md).

## 1. Episode model

A subagent is an **episodic actor**. One episode runs from arming to report;
between episodes the agent exists only as a handoff document in the archive
index.

```
spawn (non-blocking; returns handle + DM channel)
  └─ episode n: context = system prompt + handoff(n-1) + arming DM body
       working ⇄ idle          mail on the DM channel wakes (existing resident wake)
       └─ report(result)       gate: inbox drained ∧ all direct children archived
            ├─ handoff(n)      aux model call, state-only, anchors on handoff(n-1)
            └─ ARCHIVE txn     SubagentReported + HandoffCommitted + AgentArchived
                               + claim/capacity release + roster removal
                               + group-channel membership removal
                               + report mail on the DM channel → wakes parent
revive (downward DM to archived child)
  └─ AgentRestarted + AgentRegistered upsert, epoch bump, capacity check,
     arm episode n+1 with context = system prompt + handoff(n) + DM body
```

Termination signals other than a model-issued report:

| Path | Producer | Result |
| --- | --- | --- |
| Turn terminal error | engine | failure report + degraded handoff + archive |
| Budget kill (per-team turn/message) | supervisor | failed member row + degraded handoff + archive (`killed`); no report mail |
| Parent/ancestor `archive(target)` | tool call | turn cancelled (`cause: archived`) + degraded handoff + member row cancelled + archive (`archived_by_parent`); no report mail |
| Graceful drain (end of a one-shot run, SIGINT/SIGTERM, `serve` stop) | `ResidentSupervisor::drain` | turns drained, then degraded handoff + member row cancelled + archive (`shutdown`) |
| Root teardown | engine (root session delete) | request episode endings, await completion, reconcile orphans; cancelled member rows and `root_teardown` archive; no report mail |
| Handoff model call fails | engine | deterministic degraded handoff (projection-derived), marked `degraded`; archive proceeds |

An idle agent without a report is **never** auto-archived: it may be awaiting
a parent answer. A stuck child surfaces through the parent's report gate,
which rejects reporting while any direct child is live; the parent then
`archive`s it.

Every archived agent stays readable (session log, channel history,
handoff) and is **wakeable**: mail from its parent to its handle or to their
DM channel revives it (§4) under the same handle and session. A drain archives
members instead of failing them, so a later run on the same database can wake
them.

Derived invariant (used by the channel plane): **while I am live, my parent is
live** — a parent cannot archive before all its children archive. Upward mail
never resolves to an archived peer; revival is exclusively downward.

A mail wake coalesces eligible unread messages into one owned user prompt.
Delivery, `ResidentWorkStarted` (or main's `MailConsumed`) and Busy activity
commit in one fenced store transaction; user-text hooks run before that
transaction. Main synthesis also owns a user prompt. See
[atomic mail wakes](../spec/backend/task-tool.md#atomic-mail-wakes) for the
store types and examples. A failed commit leaves both prompt and consumption
absent; policy-denied mail alone never starts a turn.

Channel histories retain both participants' posts, but unread work excludes the
reader's own posts. For example, sending a task directive on one DM does not
increase `list_channel` unread counts, hold `hya exec` open after the child
archives, or retain a synthesis after all incoming results were read.
`TeamProjection::unread_incoming_mail(handle: &str, through: u64)` returns
incoming `&MailMessage` entries after the raw inbox cursor. Cursors still count
all inbox positions, including outgoing copies; no event or tool schema changes.

## 2. Unified spawn admission

Every `task` batch resolves all members, then registers them in one transaction.
The active actor claim is also its capacity lease; there is no admission journal
or in-memory spawn ledger. See [capacity and store interfaces](admission-and-governor.md).

```text
task(members[]) → bounded SpawnerPlane (full → overloaded)
→ resolve all agents, models, resources, sidecar factories and root context
→ check depth and cancellation
→ register_episodes (one transaction): parent fence + live-capacity check
   + lazy main registration + all child/member/roster/channel events + directive DM mail + claims
→ install every committed episode under claim-write serialization
→ initialize episodes concurrently; publish registration before each ending
→ reply with running handles
```

Spawn authorization and model/effort resolution live in `hya_core::resident`:
`resolve_spawn_member(&ResolveSpawnMemberCtx, SpawnMember)` returns the resolved
child specification, roster, resource policy and bound sidecar factory without
writing durable state. `resolve_main_activation_context` resolves the team's root
against the same captured binding before registration. App composition supplies
the process base, model categories, provider predicate and sidecar environment;
resident registration and permission inheritance remain in core.

Registration failure leaves no member. `per_run_budget` limits live members per
root across turns and restart. Idle and parked residents consume a lease;
archive releases it, and revival checks one additional lease inside its
transaction. Workflow preflight checks remaining capacity without reserving it.

Depth is a constant `MAX_SUBAGENT_DEPTH = 2` in `hya-core`
(`crates/hya-core/src/lib.rs` re-export). The `subagent.max_depth` config key
is removed; `SubagentLimits` keeps concurrency/budget fields only.

Mandatory parent mail bypasses bundle wake restrictions only when its sender is
the recipient's direct parent and its endpoint is their own two-member DM. Siblings, grandparents and unrelated or unknown DMs
cannot use this bypass during delivery or replay.

An episode installed with nonblank directive mail is pending immediately, before
its registration events reach the bus. `wait` therefore sees it working, and
`cancel_user_turn(child)` can hold it before its first wake. Parked Workflow
actors without directive mail stay idle. Pending member work blocks quiescence;
the later bus notification coalesces with the existing pending wake. For example,
`task` followed immediately by `wait` waits for execution rather than reporting
an initial stall. Registration still admits no user prompt: one `begin_work`
transaction owns each nonempty wake.

### 2.1 Handles: agent type + operator name (0.41.0)

A member's canonical handle is its parent's path plus a **leaf**
`<prefix>-<operator>`:

- `prefix` is derived from the `task` tool's `subagent_type` — the only way a
  caller chooses the agent. It is the resolved agent id (so
  `bundle:hya-extra/scout/agent/scout` and `scout` both give `scout`),
  sanitized deterministically: ASCII-lowercased, every run of other
  characters (`_`, `.`, `/`, spaces, non-ASCII) collapsed to one `-`, trimmed
  of `-`, capped at 32 characters, `agent` when nothing is left
  (`Acme_Scout.v2` → `acme-scout-v2`). An omitted or empty `subagent_type`
  spawns `hya-task`, so its prefix is `hya-task`. An `inline_agent` overlay's
  `name` does not change the prefix. The caller cannot pick a prefix: the
  `name` parameter was removed and a call that still passes it fails with an
  `input` error.
- `operator` is ONE name drawn uniformly at random from the checked-in
  Arknights operator list `crates/hya-core/src/handle_names.txt` (419 names,
  1–26 characters, lowercase ASCII, `^[a-z0-9]+(-[a-z0-9]+)*$`), embedded
  with `include_str!` — no network access at build or run time.

Examples: `main/hya-scout-suzuran`, `main/hya-task-exusiai`,
`main/hya-task-exusiai/hya-task-amiya`,
`main/hya-reviewer-blue-poison`.

**Name list provenance.** Snapshot of the prts.wiki operator list (干员一览,
the `data-en` attribute of every operator) taken 2026-09-25, normalized
NFKD→ASCII, lowercased, apostrophes dropped (`Ch'en` → `chen`), every other
non-alphanumeric run turned into `-` (`Blue Poison` → `blue-poison`,
`Skadi the Corrupting Heart` → `skadi-the-corrupting-heart`), deduplicated and
sorted. Nothing is filtered out.

**Never reused within a team.** A candidate leaf is rejected when it equals
the leaf of any handle the team ever registered — live roster, archived rows,
inbox owners, and channel members (all durable, read from the root
projection) — or one this process reserves for an uncommitted preparation (which also
keeps concurrent spawns apart), plus the reserved `main` and `harness`. On a
collision the draw is retried; after 16 failed single-name draws the leaf
falls back to two distinct operator names (`scout-suzuran-amiya`), then to a
deterministic sweep of every pair. Because leaves are team-unique, a bare leaf
always names one member, and mail to an archived member's handle wakes that
member — never a newer one.

**Replay.** The draw is random (SplitMix64 seeded from process entropy;
`HYA_HANDLE_SEED=<u64>` seeds it for reproducible runs, and
`SessionEngine::with_handle_rng` injects a generator in tests), but the minted
handle is recorded in `AgentRegistered` and replay reads it back — nothing
re-derives a handle. Handles are opaque strings: operator names contain
`-`, so code never parses a leaf back into prefix and name.

Workflow Stage members without an `actor` key (the one-shot runner, §10) use
the same minter with the agent id as the prefix.

## 3. Report, handoff, archive

### 3.1 The report gate

`report(result)` is a tool on the communication plane. The engine rejects it
with a typed error unless, atomically under the team lock:

1. `cursor == inbox_len` for the reporting handle (no unread mail), and
2. the handle has zero live direct children (all archived).

The **archive transaction** re-checks both conditions at commit time; a
message that committed in between refuses the archive. The refusing transaction
commits only a harness notice, so the member learns why on its next wake.

Mail the caller's current resident wake already injected into its turn
(`ResidentWorkStarted.inbox_through`) counts as read for the in-turn check.
The unread-mail rejection is actionable: it names each channel holding unread
mail (`#DM-… (N)`, or `N harness notice(s)`), the exact `read channel://<id>`
call, and the `send` / `wait` paths. Every agent can satisfy the gate because
the harness allocates the mail tools to every agent regardless of its bundle
`resource_view` (see the [coordination tools](agent-tool-surface.md#coordination-tools-allocated-at-startup)).

#### A submitted report ends the turn

Once a `report` is submitted, the engine ends the member's turn right after
the tool round that contained it: the other tool calls of that round
(before or after the `report`) still run and their results are recorded,
then the assistant message closes with exactly one `MessageFinished`
(`finish: stop`, no `cause`) and **no further model call** is made. The tool
result reads `Report submitted; it takes effect when this turn ends. …`.
Archive is decided after the turn ends. A refusal arrives as
`[mail from harness] Your report was not accepted: <reason>. Read and answer it,
then call report again.` It names unread mail, a live subagent, or an episode
change observed by the terminal transaction.

- **One submission per turn.** A second `report` in the same turn is rejected
  at the tool plane with an actionable `ToolError` ("already accepted … do not
  call `report` again") and never reaches the supervisor, so an episode
  produces exactly one `SubagentReported`. Mechanism: the turn loop hands a
  turn-scoped `hya_tool::ReportLatch` to every `report` call; an accepted
  report sets it.
- **Mail during the report round** is not steered into that round's tool
  results (the member gets no round to act on it). It stays unread, so the
  archive re-check at rest refuses the report and commits a harness notice.
  The pending report is discarded; the notice and mail wake another turn in
  the same episode. The member must handle them and submit a fresh report.
  Only a committed archive produces `SubagentReported`.
- **At rest first.** After a turn that accepted a report, the resident loop
  executes the parked report before any follow-up turn, so mail the turn
  already surfaced does not buy a fresh model round.
- A member woken later (mail to its handle or DM after the archive) starts a
  new episode and may report once more.
- The lead never reports (the supervisor rejects `report` from `main`), so its
  turns are unaffected.

### User cancellation and owed work

A completed resident turn commits `ResidentWorkFinished { session: root, handle,
epoch, inbox_through }` with its Idle activity. The reducer acknowledges that
boundary and clears the matching work. Explicit in-turn `MailConsumed` events
advance the mail cursor monotonically without settling the admitted wake.

A user cancel keeps the episode live and held, displays Idle, and retains
`resident_work` and its unconsumed inbox boundary. A later wake is owed only for
unfinished admitted work or unread mail, including a cancelled directive-mail
wake with no later mail. For example, `engine.cancel_user_turn(root)` holds that work until
`engine.resume_after_user_stop(root)` explicitly resumes it.

`begin_work(ResidentWorkPrompt::Resume { message: MessageId, append: String })`
reuses the admitted user message only while it is the transcript's last message
(empty assistant messages omitted from model requests do not count). Only new
mail is hooked and appended. Once an answer follows it, the old prompt is
immutable: the same transaction settles the old work boundary and admits one
new user message at the tail. That message contains the new mail, or
`Continue the unfinished work.` when there is none. Thus mail arriving after a
cancelled answer follows that answer in the model request and appears once.
A wake cancelled before `begin_work` commits is re-armed with its mail unread.
The coalesced user-text hook runs before the transaction and is raced with the
leased turn's cancellation, so an ending need not wait for the straggler deadline.

Forced endings and Failed episodes do not acknowledge unfinished wake mail;
their handoff retains it as Pending. A successful Reported turn has already
settled its wake; later mail refuses the report and creates another wake.
Recovery requeues all Idle pending work, including admitted directive-mail wakes,
using the same transcript rule. Work that was
Busy when the process crashed keeps the existing aborted-work policy: settle
that boundary, classify `AbortedRunning`, and queue only later mail. If its last
nonempty transcript message is still a user instruction, recovery appends an
assistant answer with `finish: cancelled`, `cause: interrupted` and explanatory
text before the next wake. This applies only to aborted running work; archives
and user-cancelled queued work do not receive a fabricated answer.
Failure to commit `ResidentWorkFinished` ends the episode through the normal
Failed ending sequence; it never parks a completed turn with unsettled work.

Sidecar death ends an episode as Failed only if loss interrupted the turn.
Loss noticed after a user cancellation disposes the dead handle while retaining
the live, user-held episode.

### 3.2 Handoff pipeline

The handoff call is the existing compaction handoff machinery with a
state-only template: `SummarizeOptions { handoff: true }`, the current episode
transcript (after its episode boundary, including local compactions) plus one
trailing prompt, executed by the **subagent's configured model**
(model policy chain unchanged; a dedicated `handoff.model`/`handoff.category`
override may point it at a cheaper tier).

Template (state-only variant of `HANDOFF_TEMPLATE`; "what is", not "how we got
here"):

```
1. Goal - the task, in the parent's terms.
2. Current state - what exists and works right now.
3. Files and code - exact paths touched, and what changed or matters in each.
4. Decisions - choices in force, including what was ruled out.
5. Pending tasks - work explicitly requested and not yet done.
6. Next step - the single next action, or `none`.
```

Each generation anchors on `<previous-handoff>` (the `previous_summary`
mechanism), so handoffs converge rather than accumulate. Episode n's context
includes only handoff(n-1) — never the transcript. In-episode context
compaction (the five-rung ladder) is unchanged and orthogonal; the terminal
handoff supersedes whatever the episode compacted internally.

Degraded handoffs (model call failed, or engine-synthesized terminality) are
derived deterministically from the ended episode: its first user message is
Goal, its last assistant text is Current state (or `no work done in this episode`),
and unread inbox bodies are Pending (otherwise the last user message). Only
Files and code and Decisions carry forward from the previous handoff, labelled
`(from generation N)`. Next step is `none`; `degraded` is always `true`.
Forced endings leave unread mail unread in history and carry it in Pending;
revival never delivers it again. Terminality never blocks on the summarizer.

### 3.3 The lead is never archived

The lead — the user-started root agent at depth 0, handle `main` — is not a
member and never goes through §3: it has no report, no handoff, and no
archive. A failed lead turn (provider/runtime error) ends with
`MessageFinished { finish: error, cause: provider_error }` and the lead's
session stays live and resumable; members keep their DM back to it.
`archive_reported_agent` refuses the root outright (`the team lead is never
archived`), and the resident loop parks a failed main slot (roster `idle`,
`current_task: "turn failed: …"`) instead of archiving it. A graceful drain
parks the lead `idle` too.

### 3.4 Leader failure

When the lead's turn fails — never on a user kill/cancel, a SIGINT, or a
drain — the harness broadcasts a wrap-up notice to the whole team:

- **Recipients:** every live resident member on the team roster, at every
  depth (roster row not `done`/`failed`, active claim), excluding the lead.
- **Channel / author:** a direct `MailSent` per member on the team-root log,
  `from: "harness"` (`hya_proto::HARNESS_HANDLE`; not an agent, never on the
  roster, bypasses the hierarchy reach rule and the channel delivery policy),
  written by `SessionStore::append_harness_mail`.
- **Body:** `LEADER FAILED: your team lead's turn ended with an error and the
  lead will not respond (<error, ≤400 chars>). Wrap up now: finish or commit
  the unit you are on, send your report, then stop. Do not start new work.`
- **Delivery:** a member mid-turn sees it through in-turn steering
  (`[mail from harness] …` on its next tool result); an idle member is woken
  for one turn.
- **Synthesis:** the dead lead gets no automatic turn — a queued `TEAM
  QUIESCED` synthesis is dropped and mail wakes of `main` wait — until the user
  resumes the lead (see [Runtime — Leader failure](runtime.md#leader-failure)).

### 3.5 Storage

Handoff documents are events (`HandoffCommitted`) on the child session log;
the archive index (§7) carries the parsed section digests for search. The
full document is replayable; `compacted_messages`-style truncation does not
apply to it.

## 4. Revival

Trigger: a DM whose recipient handle resolves to an **archived direct child**
of the sender. The write gate's resolution order is:

```
1. live roster ∩ membership+direction rules ⇒ deliver (wake)
2. own-archived direct children (archive index) ⇒ REVIVE
3. anything else ⇒ unknown (indistinguishable rejection)
```

Revive re-resolves the recorded agent's binding, spawn roster, resources and
activation-sidecar factory using the same `SessionEngine::resolve_resident_runtime`
method as startup recovery. The catalog must still contain that exact identity.
It waits only for this member's previous episode completion, outside the
claim-write lock. Team reservation and claim-write serialization cover commit
through installation, including archive and orphan-takeover decisions.

`SessionStore::revive_episode` owns one `BEGIN IMMEDIATE` transaction:

1. fence the sending parent's optional `ActorClaim` and recheck mail membership,
   direction and policy against the current projection;
2. reactivate the child's released claim with epoch + 1;
3. append `AgentRegistered { session: root, agent_session: child, handle: leaf,
   parent: Some(parent_handle), agent_type, mode: Resident }`, followed by
   `AgentRestarted { session: root, handle: canonical_path, child, epoch }`;
4. append a completed system message to the child with this exact prefix:

   ```text
   HYA_COMPACTED_CONTEXT
   PRIOR HANDOFF (generation N)
   <handoff document>
   ```

5. append the reviving `MailSent { session: root, from: parent_handle,
   to: Channel(existing_dm_id), kind, body }`.

Inputs are root/actor `SessionId`, owner `OwnerRunId`, capacity `u64`, optional parent `ActorClaim`,
sender `String`, resolved channel id `String`, `MailKind`, body `String`, context-marker
`&str`, and optional `ChannelPolicySnapshot`. It returns
`(Option<(ActorClaim, u64)>, Vec<Envelope>)` (claim and inbox cursor);
callers publish after commit and install only when a new claim is returned.
Authorization failures retain `MailAuthorizationRejected`; SQLite errors roll
back the claim, roster, boundary and mail together. No channels are recreated;
revived registration restores membership in the existing parent-unit broadcast
channel.

A newly inserted `AgentRegistered` roster row starts `resident_cursor` at the
current inbox length. The later reviving mail is therefore the first owed work;
`begin_work` records that work boundary and admits its user prompt together.
Successful completion advances the durable cursor; user cancellation retains
that prompt for resume. The model request starts at the latest context marker,
retaining one handoff and the new episode's
messages. Re-registering an already live roster row preserves its cursor.

Concurrent senders revive at most once. If another sender has already installed
the episode, the same transaction authorizes and appends ordinary mail, returning
`None` for the claim. It neither waits for nor replaces that live episode.

For example, after `archive {"target":"main/worker"}`, a parent can invoke
`send {"channel":"#DM-…","body":"Review the updated patch"}` (or use the child's
handle). The new episode sees its handoff and that mail; old handled mail stays
in history, and old unread mail is represented by Pending in the handoff.

## 5. Channel plane

### 5.1 Kinds and minting

| Kind | Id | Members | Posting | Lifetime |
| --- | --- | --- | --- | --- |
| Group | `#announce-{8}` | leader + direct reports | leader only (`send #…`) | per unit; membership ends at archive |
| DM | `#DM-{8}` | exactly two (vertical pair) | both (`send`) | created at spawn (top-down); **persists across archive/revive** |

- 8 random `[a-zA-Z0-9]` chars, collision-checked within the team root's
  channel table, re-minted on collision; minted ids are event facts
  (`ChannelCreated`), so replay is stable without derivation.
- Randomness is naming, not authorization: the write gate checks membership
  and role inside the store transaction.
- Group channels expose **no member list** through any tool. DM channels
  always expose the peer identity (derived from membership, not from input).

### 5.2 Write gate (send / report delivery)

```
send(channel?, body):
  acting identity from session context (never model input)
  handle/DM channel ⇒ private mail (child default: parent DM; archived child ⇒ revive §4)
  group channel ⇒ MailSent{Channel(#announce-…)} to live members; leader-only
  archived members are not members; nothing is queued for them
  channel omitted ⇒ role default: led unit group channel, else parent DM
  neither exists ⇒ typed error (advertised at L2, errors at runtime)

report(result):
  gate §3.1 ⇒ handoff §3.2 ⇒ archive txn §3.3 ⇒ report mail on the DM channel
```

Handles are input aliases resolved to existing sender–peer DMs at the engine
boundary. Agent mail persists only `MailEndpoint::Channel`; a missing DM rejects
the send. `^parent` and `^` select the direct parent's DM. Channel ids address
existing channels directly; named/`^name` channel resolution is removed.
`MailEndpoint::Handle` remains for harness notices only (`from == "harness"`),
enforced at the store event-append boundary. See [Agent channels](../agent-channels.md#handle-aliases-and-durable-mail)
for request, event and receipt contracts. Old databases/logs need not remain readable.

### 5.3 Wake delivery

Unchanged seam: bus `MailSent` → supervisor `on_mail` → channel members minus
sender → `pending` + notify; busy recipients coalesce into one follow-up turn.
A recipient whose session already has an active turn — including a lead whose
turn was started by `exec`/`serve` rather than by the supervisor — is busy: the
wake waits for that turn to end (single active turn per session, see
[Runtime](runtime.md#single-active-turn-per-session)). Quiescence (`TEAM
QUIESCED`) likewise fires only once no member, the lead included, has a turn
in flight, and is delivered to the lead at its next turn boundary.
New: membership excludes archived handles at fold time, and a DM whose
recipient is archived routes to the revive path before wake.

## 6. Tool surface and depth policy

| Plane | Tools | Depth 0/1 (main, L1) | Depth 2 (L2) |
| --- | --- | --- | --- |
| Orchestration | `task`, `list_agents`, `workflow`, `search_agent`, `archive` | advertised | **not advertised** |
| Communication | `send`, `list_channel`, `report` (`report`: subagents only) | advertised | advertised (group-default send errors: leads nobody) |
| Waiting | `wait` | advertised | advertised (waits on mail when it has no subagents) |
| Coding/etc. | read/write/edit/bash/… | advertised | advertised |

These planes are **allocated by the harness** when an agent starts, not
declared by its bundle: `report`, `wait`, `send`, `list_channel`, and channel
reads go to every agent (channel tools when the channel family is loaded), and
`task`/`archive` to every agent with spawn rights. A bundle `resource_view`
narrows only the coding/etc. plane; `deny` cannot remove `report`. See
[agent-tool-surface.md](agent-tool-surface.md#coordination-tools-allocated-at-startup).

Enforcement is two-layer, engine-owned:

1. **Advertisement filter** keyed on session lineage depth (engine, not
   catalog/resource policy — a bundle must not be able to reintroduce
   orchestration tools at L2).
2. **Admission depth check** (§2 step 2) as the API-level backstop.

Deleted tools: `roster`, `channels`, `join`, `leave` (folded into
`list_channel`). New tools: `dm`, `broadcast`, `list_channel`, `report`,
`search_agent`, `archive` (was `kill` before 0.41.0).

`list_channel` output: channel id, kind, can-speak, unread count, and for DM
channels the peer identity; only channels the caller belongs to; archived
peers' DM channels are excluded (archive discovery is `search_agent`).

`archive(target, reason?)` (0.41.0; replaces `kill`): stop and archive a live
descendant. The target is a handle, a caller-relative leaf, or a session id;
its live descendants archive first (deepest first). An in-flight turn is
cancelled with `cause: archived` by its episode owner. The owner awaits its
turn up to `DRAIN_DEADLINE`, closes and drops a straggler, stops its sidecar,
then commits degraded handoff → member row `cancelled` → claim release →
`AgentArchived { archived_by_parent }`. No
report mail is sent (the archiver knows). The lead is never archivable; an
unknown target lists the caller's live subagents. This is the parent's answer
to a child that blocks its report gate or is no longer needed.

`wait(targets?, mode?, timeout_secs?)` (0.41.0) blocks the caller's turn —
typically the lead's own — until its subagents finish, `any` or `all`, bounded
by a timeout (default 600 s, max 1800 s). A subagent **finishes** only by
reporting or being archived; an idle member is never finished. Each call is
measured against a baseline taken at its start: a target that had already
reported or been archived is returned under `already_finished` and never wakes
the wait (every target already finished → `nothing_to_wait_for` at once); a
member woken again by mail after its report is working until its next report
(its terminal handoff generation, bumped by every archive, tells the two
finishes apart); a member whose turn ended without a report while nothing is
queued for it — it will not continue until mailed — wakes the wait once as
`stalled` and a repeated wait blocks. Only `timeout_secs: 0` returns the
current state without blocking. The waiter subscribes to the engine bus and
re-evaluates on team-lifecycle events (`AgentActivityChanged`,
`SubagentReported`, `AgentArchived`, `MailSent`, …) against the supervisor's
in-memory slot state, so it is woken **inside** the running turn; it never
relies on a resident wake of the lead, which would queue behind that same turn
(single active turn per session). A report accepted mid-turn keeps the member
"finishing" until the archive commits. Cancelling the turn aborts the wait. The
channel-tools family overrides `wait` (explicit `overrides` in its exposure
policy) with a version that also returns on new mail for the caller — harness
mail such as `LEADER FAILED` included — reporting `woke_by: mail`. Mail is new
only past the caller's durable `MailConsumed` inbox cursor, and the wait
commits that cursor for the mail it returns (and for finished targets' report
mail), so neither a later `wait`, nor the `[NEW MAIL]` steer notice, nor a
resident wake delivers it again. The steer notice rebuilds its pending mail
from that durable cursor with the channel view refreshed, so mail on a DM
channel minted after a long lead turn began is steered too. Full contract:
[Agent tool surface](agent-tool-surface.md).

`task` schema: `resident` and `background` fields are removed; `members[]`
fan-out remains; `subagent_type` chooses the agent and names the handle
`<subagent_type>-<operator>` (§2.1; the former `name` parameter is removed and
rejected); the result carries, per member, handle + session + DM
channel id. `task_id` resume is removed (revival supersedes it).

## 7. Archive index and search_agent

A store-derived table (not folded into the live projection), keyed by team
root, one row per archived agent:

| Column | Source |
| --- | --- |
| handle, session, stable_agent_id, agent_type | registration lineage |
| description, directive_digest | spawn intent |
| goal, current_state, pending | parsed sections of the latest `HandoffCommitted` |
| degraded | handoff flag |
| archived_at, reason | `AgentArchived` |

`search_agent(query, [agent_type], [has_pending], [limit])` — scope is
**caller's direct archived children only**; structured filtering over the
section digests. The tool returns rows sufficient to choose a revive target
(handle, goal, pending, degraded, archived_at); revival itself is a `dm`.

The index is rebuildable from the event log (derived, not authoritative). It
is not part of `Projection`, keeping the live projection bounded.

## 8. Visibility matrix

| Viewer | Sees |
| --- | --- |
| Agent | its DM peers: parent (always live, §1 invariant) and live direct children; group channels as pipes without member lists. No siblings, no grandchildren, no archive. |
| Agent (via `search_agent`) | its own archived direct children as handoff digests. |
| User (client UI) | the complete agent tree at every depth, live status, and archived entries as history. |
| Event log / `Projection` | everything, globally — the single replay truth. |

The agent-side narrowing is a **read filter** over tools; the projection and
SDK `TeamProjection` mirror keep folding the full tree (conformance test
updated, not relaxed).

## 9. Event model changes

| Change | Event | Notes |
| --- | --- | --- |
| new | `ChannelCreated { session, channel, kind, members }` | `kind: Group\|Dm`; minted ids |
| new | `SubagentReported { session, root, child, handle, outcome, report }` | terminal report; workflow consumes this (§10) |
| new | `HandoffCommitted { session, handle, generation, doc, degraded }` | on the child session log |
| new | `AgentArchived { session, root, handle, child, reason }` | sole archive marker; projection removes roster row. `reason`: `reported`, `root_teardown`, `archived_by_parent` (`archive` tool), `shutdown` (graceful drain), legacy `killed` |
| new | `AgentRestarted { session, root, handle, child, epoch }` | revive marker; distinguishes from fresh spawn |
| changed | `AgentRegistered` | members use `Resident`; only the episode-less lead uses `Transient` |
| changed | `MailSent` | agents persist `Channel`; harness notices may persist `Handle` |
| retained wire type | `ChannelJoined` | no producer or reducer fold; membership comes from `ChannelCreated` |
| removed | — | `TeamEvidenceEnvelope` projection, evidence system messages, transient `MemberFinished`-on-join semantics |

`MemberFinished` is retained only as the engine-synthesized terminal marker
inside archive/kill transactions (keeps the existing tree projection
semantics); `MemberStatusChanged` is retained for running transitions.

Member row on the parent log, per resident: `MemberSpawned` in the
registration step (a `task` spawn carries the call's `description` and
`tool_call`) → `MemberStatusChanged { running }` at turn start when the row is
not already running (fenced by the resident's claim; once per episode, so an
idle resident stays `running`) → `SubagentReported` (report, or the turn-error
failure report), `MemberFinished { cancelled }` (archive, drain, stop), or
`MemberFinished { failed }` (budget kill and other failure finalization,
written in the claim-release transaction). The terminal transaction's claim and
archive fence makes retries idempotent. Forced termination replaces an earlier
episode's result even before the first revived turn; otherwise that first turn
reopens the row as `running`. Closing the parent's turn — cancel or crash
recovery — leaves rows whose `task` call already returned open.

## 10. Workflow integration

Per [ADR-0017](../adr/0017-workflow-on-unified-substrate.md), the control plane
uses the same resident episode substrate as `task`. Stage, loop worker and
verifier completion is an episode-owned `ActivationWaiter`, with explicit
`Settled { final_text }`, `Reported { outcome, text }` and
`Ended { reason, status, note }` results. The controller accepts settlement and
Done reports; it adds no Stage bus subscription or per-envelope projection poll.

Loop continuation reactivates the same member. Settlement keeps its live claim
and installed runtime; reporting releases the claim, and the next activation
revives the actor through the ordinary fenced capacity check and handoff.
Verifiers use a fresh parked resident per judgment and end it after consuming
the result, including errors and cancellation. All live workers and verifiers
count against team capacity. Run-owned actors are ended on unsuccessful exit;
root driver activity remains held through cleanup. Scheduling stays level-based.
The transient executor, member evidence envelopes and their exports are deleted.

## 11. Deletions (exhaustive)

- `run_team`, `run_pre_admitted_team*` variants, blocking joins
  (`crates/hya-core/src/subagent.rs` is deleted; `actor/` owns episodes).
- `TeamEvidenceEnvelope`, `project_envelope*`, evidence system messages.
- `uses_durable_admission_owner` routing and
  `ForegroundTransientAdmissionOwner` foreground/background reply modes (the
  preparation/intent rigor is merged into §2).
- `roster`, `channels`, `join`, `leave` tools; named user channels.
- Agent handle-targeted persistence and path-arithmetic `in_scope` (replaced by
  existing DM membership and direction gates); harness notices retain `Handle`.
- Transient member execution and mode parsing; `task_id` resume and
  `resident`/`background` task fields. The lead retains the `Transient` wire value.
- `subagent.max_depth` config key.
- Transient workflow execution path (§10).
- ADR-0002's idle-forever stance and ADR-0011's sibling scope (superseded).

## 12. Invariants checklist

1. Archive is the only exit; it is atomic with claim release, capacity release,
   roster removal, and group-membership removal.
2. The report gate (inbox drained ∧ children archived) is checked at call time
   and re-checked in the archive transaction.
3. A live agent's parent is live (upward DM never hits an archive).
4. Revival is only via downward DM from the direct parent; it bumps the claim
   epoch, consumes live capacity, and never replays transcripts into model context.
5. Every terminal path produces a handoff (model-written or degraded). Reports
   and turn/start failures commit a parent report and mail; parent archives
   close the parent member row without sending report mail. The lead has no terminal path:
   it is never reported, handed off, or archived (§3.3).
6. Active claims are capacity leases; batch registration and revival check the
   root count under the SQLite writer reservation. Root turns and startup never
   reset it. Archive releases capacity with the terminal events.
7. Orchestration tools are unadvertised at depth 2 and rejected at admission;
   the constant is not configurable.
8. Channel authorization = membership + direction, evaluated inside the store
   transaction; ids are facts, not capabilities.
9. The live projection excludes archived rows; the archive index is derived
   and rebuildable; the event log retains everything.

## 13. Implementation phases

Each phase lands TDD-first (one atomic failing test per behavior) with the
component-scoped Rust gate for the crates it touches (`cargo fmt --all --check`,
then `cargo clippy -p <crate> --all-targets -- -D warnings` and
`cargo test -p <crate>` for each touched crate and its affected dependents; see
[Development](../development.md#choosing-the-affected-components)), plus the
process gate for agent-surface phases
([docs/testing/agent-matrix.md](../testing/agent-matrix.md)). CI runs the full
workspace suite.

- **Phase 1 — lifecycle core.** Events (§9), report gate, handoff pipeline,
  archive transaction, revive, unified admission (§2), budget lease semantics,
  `kill`, root teardown force-archive. Surfaces: `hya-core` (subagent/
  resident/engine), `hya-app` (supervisor unification), `hya-tool` (task
  reshape, report tool).
- **Phase 2 — channel plane.** Channel minting/kinds, write gates, `dm`/
  `broadcast`/`list_channel`, `search_agent` + archive index, tool deletions,
  depth advertisement filter, client/SDK presentation (full tree + archive
  history).
- **Phase 3 — workflow migration.** `SubagentReported` consumption, retry =
  revive, deletion of the transient workflow path; matrix update.

## 14. Test surface impact

- `crates/hya-core/tests/turn_loop.rs`, resident/subagent unit seams: re-based
  on episode semantics (arm/report/archive/revive cycles, gate rejections,
  degraded handoffs).
- `crates/hya-app/tests/spawn_admission.rs`, `nested_spawn_tree.rs`: unified
  path only; atomic batch registration and active-claim capacity.
- `crates/hya-proto`: projection fold tests for the new events; legacy
  flat-mailbox fixture retired (breaking mandate).
- `crates/hya-e2e` p08/p09: rewritten for non-blocking task + report-driven
  completion; new cases: revive-via-DM, depth-2 tool absence, kill, budget
  capacity leases.
- Client-side team/channel presentation tests return with the future
  `hya-sdk-v1` frontend (the legacy `hya-sdk` mirror conformance suite and the
  removed TUI's presentation tests retired with those components).

### Terminal transaction implementation

The store finalizer fences the actor claim and replays the root and child inside
`BEGIN IMMEDIATE`. A prepared handoff carries its generation and observed
cursor/work boundary; a mismatch refuses termination. Voluntary reports also
recheck inbox drainage and live children here, after handoff preparation. A
voluntary refusal commits only `MailSent { from: "harness", to: Handle(member),
kind: Message, body: <reason and instruction to report again> }` and returns
`ResidentArchiveOutcome::Refused`. It leaves the claim, handoff, member row and
archive unchanged. The published mail wakes the still-live episode.
Cancellation cleanup, parent member effects, handoff, optional report/mail,
terminal activity, archive and claim release commit together. The engine calls
`publish_resident_finalization` once with the returned envelopes.
Budget kills and sidecar startup failures enter this same transition, so their
members enter the archive and can be revived. The caller supplies the handoff,
archive reason and terminal member status. Forced endings use the same numbered,
projection-derived handoff template as degraded reports, retaining an existing
handoff document. The store builds no handoff prose. Terminal failure is returned or
logged with the member session id; shutdown performs no fallback writes.

The finalizer takes `ResidentArchiveOwner::Claim(&ActorClaim)` for an owned
episode, or `Takeover` for an orphan with no in-memory episode. Takeover bumps
the active claim's epoch and replaces its owner, fences old writes, and
archives/releases in one transaction. Failure rolls
back the takeover and terminal effects together. `Committed` returns terminal envelopes; `Refused` returns only the notice. See the exact
[store and tool contracts](../spec/backend/task-tool.md#atomic-resident-termination).
Terminal activity preserves unread mail regardless of its textual note;
delivery and explicit consumption alone advance the inbox cursor.

Killed teams leave the live map immediately. Spawn uses a fresh team's counters
and triggering-turn main context; only revival of the same member awaits its old
episode. A claim committed across a kill installs into that killed team with its
ending already requested. The supervisor claim-write boundary serializes
registration commit plus installation with orphan Takeover, across team
generations; hooks and kill requests run outside it. A refused voluntary report
is discarded and the committed notice wakes the member. Root teardown requests
endings and awaits completion without member turn leases or parent report mail.

Workflow Stage waits also recognize archived actors. They use the linked parent
member row for the terminal status and summary, so failure and cancellation can
settle a Stage after the live roster entry has been removed. Live-only listings
and delivery cursors still read the live roster. `wait` reads both roster and
archive and returns failed members' current-episode reasons. Forced finalization
replaces even a previous episode's terminal member row when revival has not yet
started a turn. Claim-release idempotency prevents duplicate terminal writes.

### Transactional mail authorization

Mail ids name channels; knowing an id grants no permission. `append_channel_mail`
checks existence, sender membership and posting direction under `BEGIN IMMEDIATE`.
A group accepts posts only from its unit leader and records them as announcements.
The admitted `ChannelPolicySnapshot` send bits (`unit_leader`, `unit_member`,
`dm_parent`, `dm_child`: `u8`) are checked by the store, using the channel's actual
kind and sender role. Handle aliases resolve to DMs before this gate, so a
depth-2 child addressing its parent by relative leaf uses the same `dm_child`
send bit as an explicit DM id.
Harness-authored mail
bypasses agent send policy and still requires a live resident recipient.

For example, a parent can send to its child's `DM-…` channel; an unrelated agent
sending to that id receives `StoreError::MailAuthorizationRejected { channel,
sender, reason }` and appends no event. An authorized DM to an archived direct
child returns `MailRevivalRequired { channel, sender, target }` under the writer
lock. The engine invokes revival from that typed result after the transaction,
preserving `engine.mail_send` as the entry point. Canonical and relative-leaf
handle aliases resolve to the same persistent DM, including for revival.
The live roster wins over the archive index: during revival, registration can be
committed before `AgentRestarted`, and a DM to that live member delivers normally.

Recipient counts and replay share `TeamProjection::mail_recipient_is_live`.
For live resident DM recipients the transaction checks active actor claims in one
batch. A missing claim rejects the DM rather than recording a receipt
whose count differs from replay. Group posts use the replay predicate alone:
resident rows marked `done` or `failed` are skipped; a released claim with a
remaining nonterminal roster row does not stop the announcement or remove that
row's durable inbox delivery. Claim state controls execution ownership, not
group replay. The receipt counts all delivered inboxes, including the sender's
when subscribed. Resident-mail and follow-up capabilities still
control when an accepted inbox message wakes or steers an actor; they do not grant
permission to append mail.

Direct and harness append rejections also return
`MailAuthorizationRejected { sender: String, channel: String, reason: String }`;
their address field is `direct:<input address>`. The `send` tool exposes a mail
refusal as `ToolError::Input` (`error.type: "input"`) so the agent can correct
its request. HTTP and gRPC use the single stable API error table:
`MailAuthorizationRejected` maps to `permission_denied` (HTTP 403 / gRPC
`PERMISSION_DENIED`), while `MailboxRejected` report/episode preconditions and a
`MailRevivalRequired` without a supervisor map to `failed_precondition` (HTTP 409 /
gRPC `FAILED_PRECONDITION`). For example,
`POST /v1/sessions/<child>/steer` with `{"text":"continue"}` returns 403 for a
transient child or a resident without an active claim, with sender and address
in the diagnostic.

### Episode-owned report and archive

A resident episode has one task from registration through its ending. The task
initializes hooks, runs its mail-driven turns, and owns its sidecar and claim.
An `archive` request sets the first ending reason and cancels the episode; callers
wait on its completion watch. Dropping that wait leaves the ending running.
The task flushes its current turn before archiving. Startup and turn errors end
with `SubagentReported { outcome: failed }`, parent DM mail, a degraded handoff,
and `AgentArchived { reason: reported }`. Parent/user/Workflow archives instead
write `MemberFinished { status: cancelled }` and reason `archived_by_parent`,
without report mail. Team kill, teardown, shutdown and sidecar loss all use
this same episode-owned ending sequence. Ending a member prevents further turns
on that member; the live root may still start its quiescence synthesis turn.
Provider-entry assertions must distinguish those sessions. The running-stop
regression uses a `SessionId` notification from the provider to observe the
member's first entry and the root's synthesis entry before checking for an
extra member turn.

For example, `archive({"target":"main/hya-task-scout","reason":"stop review"})`
requests that member's ending; `send` to its DM can revive it afterward. A refused
`report` clears only the submitted report and delivers the committed harness
notice on the next wake. Every member skips a wake with no new input.

A terminal write failure resolves the completion with the store error and removes
the in-memory episode. It leaves the durable claim active, as after a crash.
The next archive takes over that orphan under the same lock held across
registration/revival's claim commit and episode installation. Takeover is never
used for a live episode. This lock is not held across initialization hooks.

A registered team member's episodes own its execution state, even while archived.
Direct prompt, command (including slash Workflow interception), and shell turns,
model/agent switches, and revert/unrevert are refused before preparation or mutation.
This prevents client turns from racing an episode or reviving an archived member.
Use `send` to the member's DM (for example, `send({"channel":"main/hya-task-scout",
"body":"review the parser"})`) or the user steer API. Mail can revive an archived
member; a direct turn cannot. Fork remains allowed: it copies a transcript snapshot
into an independent root without changing the member. Root/main sessions, sessions
never registered as members, restart restore prompts, and internal bound Workflow
entry points keep their existing behavior.

`SessionEngine::ensure_external_turn_allowed(session: SessionId) -> Result<(), CoreError>`
uses the root log's permanent `AgentRegistered.agent_session` identity, independent of
claims and the live/archived roster. External engine entry points return
`CoreError::Invalid("this subagent's episodes own its turns; steer this subagent with mail")`.
`POST /v1/sessions/{id}/turns` (`CreateTurnRequest`, prompt/command/shell kinds),
model/agent edits through `PATCH /v1/sessions/{id}` (`UpdateSessionRequest`), and
`POST /v1/sessions/{id}/revert` (`RevertSessionRequest`, including `undo: true`)
return HTTP 400 with `{ "error": { "code": "invalid_argument", "message": string } }`
and the mail guidance. The `hya.v1` gRPC methods `Turn.CreateTurn`,
`Session.UpdateSession` and `Session.RevertSession` return `INVALID_ARGUMENT`
with the same guidance. Rejected turns leave the user stop and archive state intact
and append no prompt, command, or model-switch events.
The internal completion contract is `watch<Option<Result<(), CoreError>>>`:
`None` while the episode exists, then its one terminal result after removal.
`ResidentSupervisor::end_member(root, handle)` requests an archive with note
`Workflow run cancelled`; the lifecycle `archive` tool keeps its existing
`{target: string, reason?: string}` schema and `ArchiveReceipt` result.

`ResidentSupervisor::episode_count() -> usize` counts in-memory member episodes,
excluding the lead. `ArchiveReviver::has_episode(session: SessionId) -> bool`
queries the current in-memory episode. Its internal
`reconcile_orphan(root: SessionId, handle: &str, archive: ResidentArchive) -> Result<ResidentArchiveOutcome, CoreError>` takes the
supervisor claim-write boundary, refuses an existing episode, then calls the store finalizer
with `Takeover`. `ResidentArchive` retains the store contract above; orphan
archives always use `check_gate: false` and the runtime-owning store.

The client refusal is enforced by public prompt, model-turn and shell-turn
entry points. The legacy internal Workflow executor retains its bound resume
path until Workflow moves to episode activations; it is not a client entry.
Startup recovery classifies an unread spawn directive as ordinary queued DM
mail. Admitted work retained after user cancellation resumes its existing prompt
under the same rules as any other mail wake.

The client guard checks durable registration history, so it covers archived
members, orphans and the interval between registration commit and in-memory
installation.
Public command-prompt admission uses the same guard as text prompts.


### Startup recovery composition

Before constructing the engine, composition calls
`hya_core::resident::recover_resident_claims(&SessionStore, OwnerRunId)` to fence
active claims in store order. After constructing its configured engine, it calls
`ResidentSupervisor::start_with_recovery(Arc<SessionEngine>, OwnerRunId,
&AgentSpec, Vec<RecoveredActorClaim>)`. Core terminalizes interrupted member work,
resolves the recorded identity in that session's current runtime and stages it.
This helper starts the listener without starting recovered episodes. The server
admits restore prompts and reserves their root leases before `start_recovered`;
headless commands retain the guards from `start_recovered_headless` until drain.
For example, a recovered member uses its recorded workdir and identity, without
reapplying the caller's new-spawn allowlist.

### Team ending ownership

Team activity counters live on the team object. Kill removes that object from
the live-team map immediately and requests each episode's ending. Ending episodes
remain indexed by session until completion, so same-actor revival waits for its
own previous episode while new tasks can use a fresh team and activation context.
A live team retires when its last member is gone and its lead is idle with no
work owed.

All forced endings run on the episode task after its turn has finished or reached
the cancellation deadline. Sidecar termination has a five-second deadline; failure
still releases in-memory ownership. Root deletion requests `RootTeardown`, with
no parent mail, and awaits those endings before deleting the root. Shutdown joins
intake and registration handlers before resident draining, then awaits the episode
endings and the supervisor bus listener. The lead is never archived.

The registration transaction and episode installation share the supervisor's
claim-write boundary with every orphan Takeover decision. Kill does not acquire
that boundary: a registration committed across a kill installs into its killed
team with the ending already requested. Initialization always publishes committed
registration envelopes before the episode's terminal envelopes. Cancellation
during initialization returns a failed spawn outcome. Archive publication occurs
after the episode leaves memory and before completion is resolved, preventing a
new registration from overtaking the old archive on the bus.

### Sidecar loss during an activation

The episode owns the turn future until it completes. Sidecar loss cancels the
turn's child token, requests a failed ending, and awaits the cancelled result for
up to five seconds. At that deadline it closes a straggler's message under the
same claim before dropping the future. It then stops the sidecar within another
five seconds and commits the failure report and parent DM mail, handoff, archive
and claim release. `AgentArchived` is published only after the episode has left
memory. A slow or failed sidecar stop adds its diagnostic to the ending note.

The terminal failure note is `activation sidecar lost`, the member status and
report outcome are `failed`, and the archive reason is `reported`. To continue,
the parent sends mail to that member, starting a new episode. No in-process
claim recovery occurs. An idle sidecar loss discards only the handle; the next
wake starts a fresh sidecar within the same episode, without a failure report.
Startup claim fencing still follows process restart; episode installation now waits for server restore admission. These rules add no
public tool parameters or events.

### Runtime ownership and ordered routing

The built runtime holds the supervisor strongly; the engine's resident seam
and turn observer hold it weakly. The listener also holds only a weak supervisor
reference between deliveries. Explicit runtime shutdown cancels and joins the
listener and tracks every episode, main loop and team queue worker until their
futures exit, including tasks from killed team generations. Episode completion
alone is not a task join. Drop requests each live team's ending before
cancelling it; callers await shutdown before releasing runtime resources.

The listener never awaits a team. It routes envelopes through the existing
session-to-episode-team index and current-root-team map into each team's FIFO.
Lag recovery is queued behind prior deliveries for that team. Retirement waits
for queued mail/recovery wakes as well as member endings, lead idleness,
registrations and terminal mail publication. Generic progress alone neither
holds a finished team alive nor retires a main context prepared for registration. Root-driver count and continuation ownership stay
shared across replacement teams.


### Restore admission before recovered work

Construction fences active claims, terminalizes interrupted member work and
resolves capabilities, but stages the recovered episodes without installing a
team. The daemon and in-process server share
`BuiltSessionEngine::resume_interrupted(&AppState)`: server restore admission
first, then `ResidentSupervisor::start_recovered()`. A restore reserves its
engine lease before its admission acknowledgment. Recovered children may execute
after admission, but their queued mail cannot start main before its restore
answer closes. Startup also queues recovery of root inboxes, so mail persisted
before construction is not stranded. Repeating start on an empty recovery set
starts nothing.
