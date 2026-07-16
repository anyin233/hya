# hya — Multi-Agent Runtime

`hya` is an event-sourced Rust multi-agent coding agent. This glossary defines the
ubiquitous language for its agent/team/comms model — the vocabulary that must stay
consistent across `hya-proto`, `hya-core`, the tools, and the TUI.

## Language

### Compatibility baselines

**Feature inventory**:
A human-readable compatibility baseline captured from OpenCode capabilities, with each capability prioritized and mapped to hya-native verification coverage.
_Avoid_: copy OpenCode, clone OpenCode, parity port, machine-readable coverage registry

**Must-have capability**:
A Feature inventory item that blocks the Real coding agent maturity target until implemented and covered by verification.
_Avoid_: P0, required feature

**Should-have capability**:
A Feature inventory item needed for OpenCode competitiveness but not required to prove the initial verification architecture.
_Avoid_: P1, follow-up feature

**Nice-to-have capability**:
A Feature inventory item that can wait until the core maturity target is stable.
_Avoid_: polish, someday feature

**Out-of-scope capability**:
A Feature inventory item intentionally excluded from hya's roadmap so it does not create parity churn.
_Avoid_: unsupported gap, missing feature

**Verification bootstrap**:
The initial ordered sequence of boundary-owned suites that establishes the Verification design. It starts with the tool plane before broader product flows.
_Avoid_: test order, test plan, coverage roadmap

**Registered tool contract**:
A boundary-owned behavior contract asserting that a canonical tool in the Tool plane exposes the right schema name, handles valid and invalid inputs, enforces PermissionPlane where required, and returns stable output for the model and TUI.
_Avoid_: tool unit test, tool implementation test

**Real coding agent**:
A product maturity target where feature-inventory coverage, verified toolchain behavior, interactive UX reliability, and extensibility are all required quality dimensions.
_Avoid_: prototype agent, MVP agent, clone parity

**Verification design**:
The product-level test taxonomy and run strategy that defines hya's public behavior before additional feature work is considered complete.
_Avoid_: test retrofit, ad hoc test suite, crate-only coverage

**Golden path**:
The minimal representative end-to-end flow that exercises prompt admission, a built-in tool, event persistence, projection, and TUI rendering in a single verification test.
_Avoid_: full smoke test, end-to-end demo, manual run-through

**TUI rendering contract**:
Deterministic tests over terminal frames, widgets, screens, and visible text, independent of pixel-level or platform-level rendering.
_Avoid_: visual snapshot, screenshot test, pixel test

**TUI snapshot**:
A persisted golden terminal buffer used to detect unintended changes in layout or visible output; the narrowest pixel-independent comparison layer.
_Avoid_: screenshot, image diff

**TUI interaction test**:
A test that simulates keypress flows and asserts resulting focus, mode, viewport, or overlay state changes.
_Avoid_: UI automation, manual TUI exercise

**TUI verification suite**:
The union of TUI rendering contracts, snapshots, and interaction tests covering all user-facing terminal surfaces.
_Avoid_: TUI outlook testing, TUI smoke test, TUI unit test

**Behavior contract test**:
A test asserting user-visible behavior at a stable boundary, such as a tool call, session engine rule, provider routing, store/projection, plugin host interface, or TUI surface. Distinct from a unit test per function.
_Avoid_: all function testing, unit coverage, crate integration test

**Boundary-owned suite**:
A behavior contract test suite owned by the crate that defines the stable boundary under test, with cross-crate tests reserved for product-level flows such as the Golden path.
_Avoid_: central test dump, per-function suite, global harness by default

**Provider test spectrum**:
A verification split where scripted stub providers define required CI contracts and live provider smoke tests are optional evidence outside the required gate.
_Avoid_: provider integration test, live-provider gate, recorded-only provider test

**Rust plugin binary**:
A hya plugin packaged as a Rust executable. Distinct from a Compat plugin and from in-process dynamic libraries.
_Avoid_: Rust plugin library, Rust dynamic module, cdylib plugin

**Runtime plugin registration**:
Adding a plugin to an already-running hya runtime so its declared capabilities become available without restarting the runtime.
_Avoid_: dynamic plugin load, hot load

**Hot plugin reload**:
Replacing a previously registered plugin with a new plugin instance while preserving the surrounding runtime; the new tool set is visible to the next admitted Turn and to new Sessions.
_Avoid_: runtime registration, lazy discovery

**Lazy plugin discovery**:
Deferring configured plugin startup until the plugin's capabilities are first needed.
_Avoid_: runtime registration, hot reload

**Runtime skill registration**:
Adding a skill definition to an already-running hya runtime so it becomes selectable for subsequent Turns or new Sessions.
_Avoid_: dynamic skill load

**Hot skill reload**:
Refreshing a registered skill's definition from its source without restarting the runtime; the new definition is visible to the next admitted Turn and to new Sessions.
_Avoid_: skill restart, skill recompile

**Lazy skill discovery**:
Deferring skill catalog expansion until a skill name is requested or a search is performed.
_Avoid_: eager skill load

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

**Agent catalog**:
The disk- and config-discovered set of Agent definitions available for selection or spawning. Distinct from the live **Roster**, which is projected from a running Team.
_Avoid_: roster, directory, list

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

**Session screen**:
The TUI screen for operating one active Session.
_Avoid_: normal mode

**Subagent manager**:
The TUI surface for inspecting the current Team's Roster and choosing subagent Sessions to observe.
It is scoped to the active main agent's Team, not to global Session history.
_Avoid_: global subagent browser, session browser

**Roster sidebar**:
An always-visible Session-screen sidebar block that summarizes live Roster entries for the current
Team. It complements the Subagent manager.
_Avoid_: subagent transcript sidebar, global agent list

**Subagent selector**:
The choice point inside the Subagent manager for binding one live Roster entry to a Subagent
observation view.
_Avoid_: agent picker, session picker

**Subagent observation view**:
A read-only TUI surface for observing one subagent Session without addressing it through the Prompt
composer.
_Avoid_: subagent prompt, subagent terminal


**Subagent activity row**:
A compact Transcript viewport row on the main agent's Session that surfaces subagent lifecycle
activity for a Member in the current Team. It is distinct from a child Session transcript.
_Avoid_: subagent message, child transcript inline

**Transcript viewport**:
The TUI region that renders the active Session's transcript for reading. Distinct from the prompt composer, which accepts new user input.
_Avoid_: display area, output pane

**Prompt composer**:
The TUI input surface where the user composes a prompt for the main agent. Distinct from the transcript viewport and from a stored prompt Event.
_Avoid_: input area, command box

**Queued prompt**:
A prompt temporarily held by a client surface because it cannot yet be admitted to a Session. It is
distinct from a stored Message or transcript content.
_Avoid_: queued message, submitted message

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
