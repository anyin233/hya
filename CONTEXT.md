# hya — Domain Glossary (`CONTEXT.md`)

`hya` is an event-sourced Rust multi-agent coding agent. This glossary defines
the ubiquitous language across `hya-proto`, `hya-core`, `hya-store`, `hya-tool`,
`hya-server`, `hya-relay`, and `packages/hya-tui`.

## Sessions, Agents & Bundles

**Session**:
A persisted, event-sourced interaction (`SessionId` `hysec_...`) with its own
ordered transcript in `event_log`. Every agent — root or subagent — runs as a
full Session.
_Avoid_: conversation, thread, chat

**Resume**:
Opening an existing Session in a connected client as the active interactive
session (unarchiving it if archived). Resume does not replay/re-execute turns by
itself.
_Avoid_: restart, continue

**Agent**:
A resolved role configuration — stable ID, system prompt, model/category policy,
`resource_view`, hooks, and `can_spawn` edges — with one of two **Agent origins**:
a **built-in agent** from the trusted `hya/core-agents` preset, or a **bundle
agent** from an installed/project `.hyabundle`. Legacy per-file agent markdown
files under `.hya/` or `.claude/` are not discovered directly (use Claude plugin
import or Bundles).
_Avoid_: persona, bot

**Agent origin**:
Which source an Agent came from: **built-in** (`hya/core-agents`, granted the
`Full` Harness tool plane) or **bundle** (clamped `InternalPublic` tool plane
plus its own bundle's declared resources). Origin is determined by provenance,
never by a manifest field ([ADR-0012](docs/adr/0012-one-agent-per-bundle-and-host-controlled-tool-plane.md)).
_Avoid_: kind, type

**Tool plane**:
The set of Harness-owned capabilities an Agent can see, derived from its **Agent
origin**. `Full` includes all 28 builtin tools plus Harness skills, MCP, and
plugins; `InternalPublic` includes only the builtin tool snapshot, with no
Harness skills, MCP, or external plugins. Not a security sandbox: an authorized
bundle agent may still spawn a built-in subagent that runs on its own plane.
_Avoid_: permission set, sandbox

**Bundle (`kind: Bundle`)**:
The unified deterministic package unit (`.hyabundle`, prepared format v4) parsed
and validated by `hya-bundle`. A `Bundle` may carry `agents[]` (one or many),
an optional compiled `workflow`, declarative `channels[]`, Skills, Hooks, Tools,
MCP server declarations, out-of-process sidecars (`kind: rust | bun | claude`),
custom `/v1/bundles/.../api/*` endpoints, custom `permission_modes`, and TUI
extensions (`tui:`). Historically split shapes are referred to by role as
**Plugin** (resources only, no agents/workflow), **AgentBundle** (one agent),
**AgentSetBundle** (multiple agents / channel templates), and **WorkflowBundle**
(one Workflow + reachable Agent closure).

**First-party bundle (Preset)**:
One of the twelve trusted bundles shipped with the backend (`hya/base-tools`,
`hya/extended-tools`, `hya/network-tools`, `hya/channel-tools`,
`hya/todo-tools`, `hya/core-agents`, `hya/core-skills`, `hya/core-commands`,
`hya/agent-channels`, `hya/goal-loop`, `hya/plan-impl-review`,
`hya/basic-tui-components`). Loaded from in-tree sources in Cargo builds or
`<prefix>/bundles/hya-*.hyabundle` in installed layouts.

**Agent catalog**:
The immutable set of Agent definitions available in a bound `CatalogScope`,
joining `hya/core-agents` with global installed bundles and any active Project's
`.hya/bundles`. Distinct from the live **Roster** projected from a running Team.
_Avoid_: roster, agent directory

## Episodic Subagents, Teams & Channels

**Main agent**:
The root Session at the top of a Team tree (`handle = "main"`). Direct user
prompts enter only at the main agent; inbound child report mail wakes it to
synthesize results.
_Avoid_: lead (use *unit leader* for intermediate parents), root agent

**Subagent (Member)**:
A child Session spawned via `task` (or a Workflow Stage). Since `0.41.0`
([ADR-0015](docs/adr/0015-unified-resident-subagent-lifecycle.md)), **every
spawned subagent is an episodic resident actor** (`MAX_SUBAGENT_DEPTH = 2`). Use
*member* for spawn/supervision mechanics (`MemberId`, `MemberInfo`) and
*subagent* for the agent concept.
_Avoid_: worker thread, blocking transient subagent (retired)

**Episode**:
One bounded activation of a resident subagent from spawn (or downward DM
revival) until `report` (or `archive` / drain / budget kill). On `report`, the
engine generates a state-only **Handoff** anchored on the prior handoff, refunds
its `SubagentGovernor` budget lease, releases its `ActorClaim`, and archives the
subagent. A later downward DM from its parent revives the same handle and
Session into episode `n + 1` with `system prompt + handoff(n) + DM body`.

**Turn**:
One admit-prompt → provider stream (with 0+ tool-call rounds) → `MessageFinished`
cycle on a Session. Idle resident subagents consume zero tokens and run one turn
per coalesced mail wake.

**Quiescence**:
The team-scoped state in which every Session in the Team is idle and no mail is
in flight; its transition wakes the main agent when unconsumed team work awaits
synthesis.

**Team**:
The Session tree rooted at one root Session. Team roster, channels, and mail
events are appended to the **team-root** Session's `event_log`
([ADR-0001](docs/adr/0001-event-sourced-mailbox-and-channels.md)).
_Avoid_: swarm, crew

**Handle**:
The team-scoped canonical path of an agent (`main`,
`main/hya-scout-suzuran`, `main/hya-task-exusiai/hya-task-amiya`). Leaf names
are minted deterministically as `<sanitized-subagent-type>-<operator>`
([subagent-orchestration.md §2.1](docs/architecture/subagent-orchestration.md#21-handles-agent-type--operator-name-0410)).
_Avoid_: bare agent name, caller-chosen name

**Channel & Mail**:
Event-sourced inter-agent communication ([ADR-0016](docs/adr/0016-channel-communication-plane.md)).
Addressing is strictly **vertical** (parent ⇄ direct child; siblings are
invisible to one another) over two event-minted channel kinds:
- **Group channel (`#announce-{8}`)**: one per unit; only the unit leader may
  broadcast, reaching live direct reports with no member list exposed.
- **DM channel (`#DM-{8}`)**: one per parent/child pair, minted at spawn and
  persisted across archive/revive cycles; downward DM to an archived child
  revives it.
_Avoid_: sibling DM, user-named `#topic` join

**Roster**:
The projected live directory of Team members (canonical handle, agent type,
`RosterStatus`, `ActorEpoch`, harness heartbeat freshness), folded from the
team-root `event_log`.

## Workflows, Goal Mode & Loop Mode

**Workflow**:
A named, revisioned, acyclic graph of **Stages** authored in `*.hya.md`
(YAML frontmatter + `flowchart TD` topology), compiled by `hya-workflow`,
admitted/inspected through `hya_app::WorkflowControl`, and executed on the
unified resident subagent substrate by `hya-core`
([ADR-0013](docs/adr/0013-user-assembled-agent-workflows.md),
[ADR-0014](docs/adr/0014-workflowbundle-and-event-sourced-control.md),
[ADR-0017](docs/adr/0017-workflow-on-unified-substrate.md)).
_Avoid_: preset pipeline, macro, script

**Workflow program**:
A JavaScript async function body that the **Orchestrator agent** submits
through the `workflow` tool of `hya-extra/orchestrator`. Its only effects are
starting specialised workers (`agents.run`) and asking a judge model
(`models.classify`); the bundle runtime executes it and writes one trace
record per effect. Stored by name and reused with new `args`. Not a
**Workflow**: it is not compiled, has no Stages, and is authored by an agent
at run time ([ADR-0032](docs/adr/0032-workflow-programs-and-the-orchestrator-agent.md)).
_Avoid_: workflow (unqualified), script, code-mode program

**Observation**:
The report of a worker that only read. An `orchestrator-scout` answer made on
an unchanged **Workspace state** is remembered and reused for the same
question; the reusing trace record names the session that made it.
_Avoid_: cache entry, memo

**Workspace state**:
The value of the `workspace.state` host capability: a SHA-256 identity of the
Git work tree containing a session's working directory (its location, HEAD
tree, status, and the content of status-listed files; ignored files and
top-level `.hya/` excluded). Equal values mean the same checkout with the same
content.
_Avoid_: snapshot, revision

**Stage**:
One node in a compiled Workflow DAG: target Agent, directive (`{{input.<name>}}`
plus bounded predecessor evidence), optional `actor` key binding sequential
Stages to one resident Session, optional loop verifier, and optional
**Workflow model assignment** (`model`, `reasoning`, ordered `fallback`
candidates, emitting bounded `WorkflowStageRouteOutcome` events).

**Workflow identity & Workflow run**:
`WorkflowIdentity` captures source, name, and immutable revision (folding both
graph source and owning bundle digest). Selecting a Workflow records its
identity on a Session without starting execution; a `WorkflowRun` (`wfrun_...`)
executes the DAG to `succeeded`, `failed`, `cancelled`, or `interrupted`.

## Projects, Daemon & Remote Access

**Backend daemon**:
The detached `hya serve` process owning a SQLite database via
`<db>.runtime-owner.lock` and `<db>.server.json`
([ADR-0022](docs/adr/0022-one-writer-per-database.md),
[ADR-0023](docs/adr/0023-persistent-backend-daemon.md)). Outlives terminal and
browser clients; controlled via `hya serve start|status|stop|restart`.

**Project & Project root**:
A named client-chosen workspace (`prj_...`) consisting of one or more ordered
absolute **Project roots** on the backend host
([ADR-0024](docs/adr/0024-project-model-and-client-chosen-workspace.md)). File
tools access paths inside any root of the session's Project without asking and
escalate outside paths as `ExternalDirectory` asks
([ADR-0026](docs/adr/0026-multi-root-path-permissions.md)).
_Avoid_: server cwd, workspace folder

**Catalog scope**:
The `Global`, `Directory(path)`, or `Project { id, roots }` key determining a
request's or session's merged bundle, plugin, skill, command, and `AGENTS.md`
catalog ([ADR-0027](docs/adr/0027-per-project-catalog-tier.md)). Only a
registered `Project` scope spawns project-local `.hya/bundles` and
`.hya/plugins` processes.

**Temporary session vs Ephemeral session**:
- **Temporary session**: A Session (`kind: temporary`) belonging to no Project,
  isolated in `$XDG_CACHE_HOME/hya/scratch/<session_id>` (never auto-deleted).
- **Ephemeral session**: A newly created root Session (`ephemeral: true`)
  opened speculatively by a TUI on connect or `/new`; if still unused, the
  daemon reaps it ~5 s after no client stream watches it.

**Relay, Room, Relay link, Bridge & Host connector**:
Components of end-to-end encrypted remote control
([ADR-0025](docs/adr/0025-secure-relay.md)):
- **Relay (`hya proxy`)**: Stateless blind forwarder of `hya.relay.v1` frames
  over gRPC or WebSocket; holds no keys and sees only Room IDs and ciphertext.
- **Room**: In-memory rendezvous keyed by `room_id` derived from the backend's
  Ed25519 public key.
- **Relay link**: `hya://<host>/<room_id>?t=<auto|grpc|ws>#<x25519_pub>.<psk>`
  capability URL; holding it grants full backend access (`hya serve relay rotate`
  revokes it).
- **Bridge (`hya bridge` / `hya --connect` / `/connect-remote`)**: Client-side
  loopback HTTP listener that wraps each connection in a `Noise_NKpsk0` stream
  so the TUI remains a plain `hya.v1` HTTP client.
- **Host connector**: `hya-server::relay_host` component inside `hya serve` that
  registers the Room, terminates Noise as responder, and dispatches decrypted
  requests into the `hya.v1` router tagged with `Origin::Relay`.

## Client Surfaces & Models

**TUI & WebUI**:
`packages/hya-tui` is the sole interactive OpenTUI terminal client
([ADR-0019](docs/adr/0019-adopt-opentui-frontend.md)); `packages/hya-tui-web`
hosts that exact TUI on a real PTY and renders it in browsers via xterm.js as
the WebUI ([ADR-0018](docs/adr/0018-browser-rendered-tui-test-environment.md),
[ADR-0020](docs/adr/0020-bundle-tui-and-webui-in-hya.md)). Core TUI regions
include the **Transcript viewport**, **Prompt composer** (with client-side
**Queued prompts** while a turn is busy), left **Projects sidebar**, right
**Sidebar** (Session, Model/Context, Permissions, Workflow, Todo, MCP),
**Subagent panes** (`/subagents`), and full-screen management views (`/project`,
`/key` Provider View, `/agent`, `/bundles`, `/mcp`, `/rules`, `/diff`,
`/extensions`, `/api`).

**Category & Candidate**:
A logical model name in `config.yaml` `categories:` mapped to an ordered list of
concrete `provider/model` **Candidates** resolved by failover at spawn time
([ADR-0004](docs/adr/0004-model-category-resolution-and-precedence.md)).
Distinct from a `#variant` reasoning suffix.
