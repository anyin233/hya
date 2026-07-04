# hya — Multi-Agent Runtime

`hya` is an event-sourced Rust multi-agent coding agent. This glossary defines the
ubiquitous language for its agent/team/comms model — the vocabulary that must stay
consistent across `hya-proto`, `hya-core`, the tools, and the TUI.

## Language

### Agents & sessions

**Session**:
A persisted, event-sourced conversation with its own transcript. The unit of persistence
and TUI navigation. Every agent runs as a session.
_Avoid_: conversation, thread, chat

**Resume**:
Opening an existing Session in the connected runtime as the active interactive session. Resume
does not create a new Session, and it does not imply rerunning or continuing a model Turn. If the
Session is unavailable, no Session has been resumed.
_Avoid_: restart, replay, continue

**Agent**:
A role/config — name, system prompt, model or category, tools, permissions — sourced from a
built-in ("native") agent or a user-authored markdown file. Distinct from the Session that runs it.
_Avoid_: role, persona, bot

**Subagent**:
A child session spawned by another agent. Every subagent *is* a full Session, not a lightweight
object. In the runtime code the spawned unit is called a **Member**.
_Avoid_: worker, task (a task is the work, not the agent), sub-session

**Member**:
The runtime representation of a subagent within a team spawn (`MemberSpec`, `run_member`). Use
"member" when talking about the spawn/orchestration mechanics; "subagent" when talking about the
concept.

**Main agent**:
The root session — the top of a team tree. The user types *only* to the main agent. It is also an
actor: woken by child mail to synthesize the team's result. Its TUI window can never be closed.
_Avoid_: orchestrator (that's its role, not its name), lead, root agent

### Lifecycle

**Transient subagent**:
The default lifecycle: spawn → run one turn → return a bounded summary → dormant. The parent
**blocks** until it finishes. Used for fire-and-forget task work.
_Avoid_: one-shot, ephemeral (reserved for inline agent *definitions*)

**Resident subagent**:
An opt-in lifecycle: a long-lived, addressable, event-driven **actor**. Idle at zero token cost;
woken by inbound mail (never by direct TUI input) to run exactly one turn, then returns to idle.
_Avoid_: daemon, persistent agent, background agent

**Turn**:
One admit-prompt → stream → tool-calls cycle of a session. A resident runs exactly one turn per
wake. "Alive" for a resident means "its session persists and mail can trigger a new turn" — not a
continuously running loop.

**Quiescence**:
The state in which a team is done: every session is idle *and* no mail is in flight. Detected
team-scoped; its zero-transition wakes the main agent to synthesize.
_Avoid_: done, finished, settled

### Teams & comms

**Team**:
The tree of sessions rooted at one top-level run. The scope for handle discovery, budgets, and
quiescence. Team comms are appended to the **team-root** session's event log.
_Avoid_: swarm (swarm is the behavior, not the unit), group, crew

**Handle**:
A stable, human-friendly, team-scoped agent name (e.g. `reviewer-3`, or `main` for the root). The
address for mail and the label in the TUI. Assigned deterministically at spawn (`{type}-{ordinal}`)
and bound to a session via an `AgentRegistered` event.
_Avoid_: name (overloaded with agent name), address, id

**Mail**:
A message from one agent to another, addressed by handle (direct 1:1) or by `#channel`. Every mail
is a `MailSent` **Event** in the log, folded by the shared projection into recipient **inboxes**.
_Avoid_: message (too generic — a model message is different), DM

**Channel**:
A named (`#name`) multi-subscriber endpoint. Sending to a channel fans out to every current
subscriber's inbox. **Broadcast** is a well-known channel everyone joins. One delivery mechanism
underlies both direct mail and channels.
_Avoid_: room, topic, group chat

**Roster**:
The team-scoped directory of live agents (handle, type, status, current task), read from the
projection — never from disk.
_Avoid_: directory, registry, member list


### Surfaces

**TUI**:
The terminal user interface surface for operating sessions. Distinct from server APIs and client
runtimes.
_Avoid_: terminal, UI (too broad)

**Legacy TUI**:
An older terminal user interface surface. Use this term only when contrasting that older surface
with another TUI surface; it does not mean every terminal renderer or client.
_Avoid_: TUI (too broad), all terminal UI

### Models

**Category**:
A logical model tier named in an agent file (e.g. `deep`, `quick`). Resolves to a concrete
`provider/model` at spawn time via an ordered candidate list with failover to the first
configured/healthy provider. Distinct from a **reasoning variant** (the `#variant` suffix on a
concrete model ref).
_Avoid_: tier (the old `tier-*` placeholders are removed), profile, model alias

**Candidate**:
One concrete `provider/model` ref in a category's ordered preference list. Candidates express
**failover** (try the first that is servable), not a load-balancing pool.
_Avoid_: fallback (that's the tail of the list, not the whole set), option
