# Design Risk Draft: yaca Ambitious v0

## Scope Lens

This draft assumes the confirmed decisions are fixed:

- **D1:** minimal breadth, full depth.
- **D2:** Anthropic, OpenAI, and OpenAI-compatible/local providers in v0.
- **D3:** client/server architecture from day 1.
- **D4:** full omo-style team mode plus full goal lifecycle parity in v0.

Given those decisions, the main risk is not that any single feature is impossible.
The main risk is composition: provider streaming feeds tool execution, tool execution
feeds persisted transcript state, team mode deliberately hides child transcripts from
the lead, and the goal evaluator can only judge the visible transcript. If those
interfaces are not designed and tested first, the project can appear to work in
single-agent demos while failing the exact differentiators that justify yaca.

The survivable strategy is therefore: build a thin vertical slice first, but make
the slice cross every dangerous seam. Stub breadth, not seams.

## Ranked Risk Register

### 1. Cross-provider tool-call and streaming normalization

**Why risky:** D2 requires multiple provider families in v0, but Anthropic,
OpenAI, OpenAI-compatible gateways, and local providers differ in streaming chunk
shape, tool-call semantics, partial JSON handling, reasoning streams, usage
accounting, finish reasons, abort behavior, and retry/error surfaces. If the
agent loop or TUI sees provider-native events, every later feature inherits this
variance.

**Failure mode:** Tool calls are parsed incorrectly or late; partial argument
deltas corrupt JSON; the TUI shows duplicated or missing parts; the permission
engine approves a different command than the model actually emitted; goal
evaluation sees a transcript that differs by provider; adding the second provider
forces rewrites across the agent loop, session store, and renderer.

**Mitigation:** Treat provider normalization as the first critical subsystem, not
as plumbing. Define one canonical internal stream before implementing real agent
behavior: `TurnStarted`, `AssistantMessageStarted`, `TextDelta`,
`ReasoningDelta`, `ToolCallStarted`, `ToolCallInputDelta`, `ToolCallReady`,
`ToolResult`, `ToolError`, `AssistantMessageFinished`, `UsageDelta`, and
`TurnFinished`. Split adapters into `Provider` (vendor/model catalogue),
`Protocol` (wire semantics), and `Route` (endpoint/auth/transport). Add a
provider conformance suite with recorded fixtures for streaming text, one tool
call, multiple tool calls, malformed partial tool JSON, model refusal, provider
abort, and usage reporting. The agent loop and TUI must only consume canonical
events.

**Front-load proof:** Before team mode or a rich TUI exists, run the same scripted
turn through at least two real provider routes plus a deterministic fake route
and assert identical canonical event sequences modulo provider ids, token counts,
and timestamps.

### 2. Transcript-only goal evaluator versus team-mode transcript isolation

**Why risky:** D4 combines two features whose incentives conflict. Team mode saves
tokens by keeping child-session transcripts out of the lead transcript. Goal mode
judges only the visible transcript and cannot call tools or read child state. If
team results remain in mailbox/task-board state only, the evaluator cannot know
whether the team completed the work.

**Failure mode:** Goals loop forever because the evaluator cannot see hidden team
evidence; worse, goals pass because the lead claims success without surfacing the
actual evidence. Team mode demos pass, goal demos pass, but a goal-driven team
turn is unjudgeable.

**Mitigation:** Make a **Team Evidence Envelope** a first-class lead-visible
artifact. Every team turn that contributes to a goal must project a bounded,
structured synthetic message into the lead transcript: team id, member ids,
assigned tasks, final task statuses, member result summaries, commands/checks run,
exit codes, changed files, unresolved blockers, and links to child session ids.
The envelope must not include full child transcripts, but it must include enough
evidence for a transcript-only evaluator. The goal evaluator consumes a
`TranscriptProjection`, not raw database rows, so tests can assert exactly what is
visible to the gate.

**Front-load proof:** Build a fake team turn where child transcripts contain the
only detailed work. Assert the evaluator receives only the lead transcript plus
the evidence envelope, cannot access child messages, and still reaches the right
yes/no decision based on surfaced evidence.

### 3. In-server async subagent supervision and failure containment

**Why risky:** D3 keeps orchestration inside the server in v0, and D4 requires
parallel child sessions. A naive `tokio::spawn` model lets one panicking,
looping, or backpressured subagent degrade the whole server. Cancellation and
shutdown are especially dangerous once tools, provider streams, tmux, worktrees,
and SQLite writes are all active.

**Failure mode:** A child task panic aborts shared state; a hung provider stream
keeps a team forever active; force-delete leaves orphaned tasks; cancellation
drops a final tool result but leaves the TUI showing a running tool; one noisy
subagent starves the server event loop.

**Mitigation:** Introduce a supervision layer before full team mode. Each session
or team member gets a `SessionActor` with bounded input/output channels, a
`CancellationToken`, heartbeat/last-event time, explicit terminal states, and a
supervisor-owned join handle. Panics are caught at task boundaries and converted
to failed session events. Team shutdown and force recovery operate through typed
state transitions, not ad-hoc message conventions. Bounded queues and per-team
budgets provide backpressure.

**Front-load proof:** A stress test starts one normal child, one child that
panics, one child that loops until cancelled, and one child whose output channel
fills. The server remains responsive, team status is accurate, and force recovery
terminates all children deterministically.

### 4. Permission model correctness under concurrency

**Why risky:** yaca needs an allow/ask/deny permission plane while multiple
subagents may request shell/edit/write permissions concurrently. Child sessions
derive permissions from parents, but team members must not become confused
deputies that use the lead's authority beyond their scope.

**Failure mode:** An `always allow` approval intended for one command leaks to
another child; approvals race with tool execution; stale snapshots allow a tool
after the user revokes a rule; local worktree paths are confused with the main
workspace; a background member blocks forever waiting for a user question it is
not allowed to ask.

**Mitigation:** Centralize policy evaluation in a `PermissionEngine` actor and
make approvals addressable by request id, session id, tool call id, action, and
resource scope. Each turn receives an immutable permission snapshot; persistent
`always` changes only affect later turns. Child permissions are derived by an
explicit function that can only narrow or add child-specific external-directory
grants. Team members have question-permission denied by construction. Add
property tests for last-rule-wins, scope matching, snapshot immutability, and
child derivation.

**Front-load proof:** Simulate two subagents requesting overlapping shell/edit
scopes at the same time. Verify one approval cannot satisfy the wrong request,
revocation affects the next turn only, and child denies cannot be bypassed by
parent allowances.

### 5. SSE/client reconnect and partial-stream rendering

**Why risky:** D3 makes the TUI a client of a local server, and N1 requires smooth
streaming. SSE is simple, but reconnect, replay, and partial message rendering
are hard once the stream includes text deltas, reasoning deltas, tool lifecycle
events, permission prompts, and team events.

**Failure mode:** Reconnect duplicates text, loses a `ToolCallReady`, leaves a
tool stuck as `running`, or replays events out of order. The TUI looks flaky even
when the server is correct, and goal transcripts diverge from what users saw.

**Mitigation:** Make all client events durable, ordered, and reducible. Every
event has a monotonically increasing `event_id`, session id, turn id, and part id.
The client reducer must be idempotent. SSE reconnect uses `Last-Event-ID` to
resume from the event log; if the gap is unavailable, the server sends a snapshot
plus a new stream cursor. Do not render provider deltas directly; render projected
message/part state.

**Front-load proof:** Run a scripted stream, disconnect after each event class,
reconnect, and assert the final TUI/message projection equals the uninterrupted
projection.

### 6. SQLite write contention with concurrent sessions

**Why risky:** Event-sourced persistence is the right shape, but team mode can
create many sessions writing message parts, tool updates, mailbox events, usage
events, and projections at once. SQLite can handle this if writes are serialized
well; it becomes painful if every async task writes directly.

**Failure mode:** `database is locked` errors under normal team load; provider
streams block on database writes; projections lag or diverge from event log;
resume misses events; tests are green single-agent but fail under concurrent
subagents.

**Mitigation:** Use WAL mode and a single async database writer actor for event
appends and projection updates. Prefer append-only event writes on the hot path;
batch projection writes where possible. Tool outputs and provider deltas should
not hold write transactions open while awaiting I/O. Add `busy_timeout`, explicit
retry policy for transient lock contention, and metrics for queue depth and write
latency. Keep in-memory team control state separate from durable session events,
then persist team snapshots at safe boundaries.

**Front-load proof:** A concurrency test runs multiple fake child sessions that
emit provider deltas and tool events at high rate. It must complete without lock
errors, preserve per-session event order, and recover the same projection after
replay.

### 7. Git-worktree and tmux integration fragility

**Why risky:** D4 requires worktree-per-agent and tmux panes, but these are
external state machines with platform quirks, cleanup hazards, and strong
filesystem consequences. They are also not needed to prove the core orchestration
algorithm.

**Failure mode:** Members operate in the wrong directory; worktrees are orphaned;
cleanup removes a user-owned path; tmux panes outlive sessions; non-tmux
environments fail the entire team feature; a force recovery cancels the agent but
leaves a dirty worktree unreported.

**Mitigation:** Put worktrees and tmux behind `WorkspaceRuntime` and
`PaneRuntime` traits. Start with a no-op/in-process runtime for early phases,
then add real integrations with a registry of owned resources, path allowlists,
leases, dry-run planning, stale-resource discovery, and explicit cleanup states.
Never infer ownership from path shape alone; persist resource ids and creation
metadata. Treat tmux as observability/control, not as the source of truth.

**Front-load proof:** Before enabling real agent work in worktrees, run a temp-repo
integration test that creates, lists, marks dirty, cleans, and force-recovers
worktrees without touching the main checkout. Run tmux tests only when tmux is
available; otherwise the feature degrades with a clear capability error.

### 8. Context and token accounting across providers, teams, and goals

**Why risky:** Token efficiency is a differentiator, but providers report usage
differently and local providers may not report it at all. Team mode hides child
transcripts; goal mode can loop turns; summaries can silently grow. Without a
central ledger, yaca cannot prove the benefit or enforce budgets.

**Failure mode:** Goal loops burn tokens without clear caps; team mode looks cheap
because child usage is omitted; model routing chooses expensive models for cheap
tasks; context windows overflow because summaries are not counted; cost display
differs by provider.

**Mitigation:** Add a `TokenLedger` from the first agent loop. Record actual usage
when provider-supplied, estimated usage otherwise, and mark the confidence level.
Ledger entries attach to session id, turn id, provider, model, route, team id,
goal id, and category. Enforce per-turn, per-goal, and per-team budgets. Require
every team evidence envelope and goal status to include aggregate child usage.

**Front-load proof:** A fake goal-driven team run with mixed provider routes must
produce a budget report that includes lead usage, child usage, evaluator usage,
estimated local usage, and stop reason.

### 9. Canonical tool schema plus typed runtime wrapper

**Why risky:** The tool system must serve model-facing schemas, runtime permission
checks, typed execution, TUI lifecycle, and transcript projection. Provider tool
schema support is uneven, especially for OpenAI-compatible/local routes.

**Failure mode:** Tool inputs validate differently by provider; model-facing
schemas drift from runtime structs; permission checks run after side effects;
tool outputs are impossible to summarize consistently; adding `team_*` tools
duplicates infrastructure.

**Mitigation:** Define tools once as typed Rust structs with generated JSON schema
and a runtime wrapper that performs decode, permission check, execute, output
validation, event emission, and transcript projection in that order. Keep the v0
essential tool set narrow, but make orchestration tools use the same wrapper as
read/write/shell.

**Front-load proof:** One provider-streamed tool call goes through schema decode,
permission ask/allow, typed execution, canonical tool lifecycle events, persisted
result, and transcript projection with no provider-specific branches.

### 10. Team control-plane state machine complexity

**Why risky:** Full omo parity means mailbox, task board, team status, lifecycle,
shutdown request/approve/reject, force delete, lead-only operations, and member
contracts. This is too much to leave as loose shared structs.

**Failure mode:** Teams can be deleted while members are active; member status and
task status disagree; shutdown kinds leak through ordinary messages; lead-only
broadcast is bypassed; background handles and session ids are confused.

**Mitigation:** Model team state as an explicit state machine with typed commands
and invariants: `Creating`, `Active`, `ShutdownRequested`, `Draining`,
`Completed`, `Failed`, `ForceDeleting`, `Deleted`. Store both `background_task_id`
and `session_id` wherever a member launch is recorded. Reject invalid transitions
at the command boundary. Keep mailbox/task board behind a `TeamControlPlane`
trait so in-memory actor and future file-backed variants share semantics.

**Front-load proof:** State-machine tests cover all 12 tool commands, invalid
lead/member permissions, graceful shutdown, rejected shutdown, active-member
delete rejection, and force recovery.

### 11. Goal loop runaway and evaluator drift

**Why risky:** A model-based evaluator is intentionally independent, but it can
be wrong, inconsistent, or too trusting. Because `/goal` auto-starts new turns,
one wrong setup can create a costly loop.

**Failure mode:** The evaluator repeatedly says no for an already met condition,
says yes based on claims instead of evidence, exceeds provider quotas, or resumes
with stale baselines. Non-interactive mode can run too long without useful output.

**Mitigation:** Require hard turn/time/token caps even when the condition omits a
bound. Use a `GoalEvaluator` trait with a deterministic evaluator for tests and a
model evaluator for production. The evaluator prompt must distinguish evidence
from claims and require command outputs or structured team evidence for yes.
Persist goal state and reset resume baselines exactly as documented.

**Front-load proof:** Unit tests cover met, unmet, false-claim, cap reached,
resume, clear, and non-interactive cases using deterministic transcript fixtures.

### 12. Local/OpenAI-compatible provider capability variance

**Why risky:** OpenAI-compatible does not mean OpenAI-equivalent. Local routes may
lack tool calls, streaming usage, reasoning fields, reliable JSON mode, or stable
finish reasons.

**Failure mode:** The app advertises local support but fails on any tool-using
agent turn; model routing selects a route that cannot run team members or the
goal evaluator; errors appear deep inside the loop instead of at route selection.

**Mitigation:** Add a capability matrix per route: `streaming_text`,
`streaming_tool_calls`, `parallel_tool_calls`, `usage_reporting`,
`json_output`, `reasoning_stream`, `abort`, and `max_context`. Route selection
must reject incompatible tasks before the turn starts. Local support can be v0 if
the route passes the required capability subset or is clearly limited to non-tool
roles.

**Front-load proof:** Route-selection tests attempt to assign incompatible local
routes to shell-using agents, team members, and goal evaluators and receive clear
preflight errors.

## Critical Path: Thin Vertical Slice First, Then Widen

The fastest de-risking path is not to build all v0 features shallowly. It is to
build one end-to-end loop that crosses the dangerous seams: provider streaming,
tool call, permission, persistence, SSE, transcript projection, goal evaluation,
child-session evidence, and cancellation. Features that do not exercise those
seams should wait.

### Phase 0: Contract skeleton and conformance harness

**Purpose:** Freeze the core contracts before implementation pressure causes
provider-specific or UI-specific leakage.

**Build:** Define the crate boundaries and core traits/types: canonical event
stream, message/part lifecycle, provider/protocol/route traits, tool wrapper,
permission request model, event store interface, transcript projection,
goal-evaluator trait, team-control-plane trait, supervisor interface,
client-event reducer, and token ledger entries. Implement deterministic fake
provider, fake tool runtime, in-memory event store, and reducer tests.

**Stub:** Real providers, real SQLite, rich TUI, real team members, real worktrees,
and tmux.

**Validation gate:** `cargo test --workspace` passes contract, reducer,
permission, and transcript-projection unit tests. A fixture stream reduces to one
assistant message with text plus one completed tool part.

**Rollback point:** If the contracts become too broad, cut optional event kinds
such as reasoning deltas and usage deltas from the first reducer, but do not cut
tool lifecycle events, transcript projection, permissions, or event ids.

### Phase 1: Provider/tool/SSE walking skeleton

**Purpose:** Prove the core agent loop and client/server boundary before adding
team mode.

**Build:** A local server owns one session. A minimal client connects over SSE.
One fake provider and one real provider stream a turn that emits text, requests a
single harmless tool, receives a tool result, and finishes. The server persists
canonical events, projects message state, emits client events, and can replay the
session after restart. Permission flow can be pre-approved for the skeleton but
must still pass through the permission engine.

**Stub:** Rich ratatui layout, multi-provider breadth, team tools, goal loop,
worktrees, tmux.

**Validation gate:** Run a dev fixture such as
`cargo run --bin yaca-server -- --dev-fixture provider_tool_turn` and connect a
minimal client. The observed behavior: streamed text appears once, the tool
transitions pending to running to completed, the final transcript survives
restart, and reconnect from a stored event id produces the same final projection.

**Rollback point:** If real SSE reconnect is unstable, keep the HTTP/SSE API
shape but temporarily run the client in-process against the same reducer while
the event-id/replay bug is fixed. Do not let provider callbacks bypass the
canonical event stream.

### Phase 2: Multi-provider normalization hardening

**Purpose:** Front-load D2 before upper layers depend on accidental behavior from
one vendor.

**Build:** Anthropic, OpenAI, and OpenAI-compatible/local route adapters behind
the same provider/protocol/route split. Add route capability discovery and
preflight checks. Create recorded fixture tests for text streaming, tool input
streaming, parallel/multiple tool calls if supported, malformed partial JSON,
finish reasons, aborts, and usage reporting.

**Stub:** Team mode still uses fake providers unless a real route passes the
capability matrix. Goal evaluator can remain deterministic.

**Validation gate:** The provider conformance suite runs the same semantic cases
against all enabled routes. The internal agent loop test is provider-parametric:
no provider-specific branches outside adapters.

**Rollback point:** If OpenAI-compatible/local routes fail tool-call capability,
ship them behind explicit capability labels and restrict them to non-tool or
evaluator roles until they pass. This preserves D2's abstraction without allowing
unsupported routes to corrupt agent turns.

### Phase 3: Goal engine before real team complexity

**Purpose:** Prove transcript-only verification while the transcript is still
simple enough to reason about.

**Build:** Goal state, set/status/clear commands, turn-end evaluator hook,
hard turn/time/token caps, reason-feedback loop, resume behavior, and
non-interactive mode. Use deterministic evaluator fixtures first, then one cheap
model route. The evaluator consumes `TranscriptProjection` only and has no tool
access.

**Stub:** Team-created evidence can be a synthetic fixture; no real team members
yet.

**Validation gate:** A scripted goal run loops once on an unmet transcript, feeds
the evaluator reason into the next turn, then clears after a transcript with an
explicit command result. False claims without evidence do not pass.

**Rollback point:** If model evaluation is flaky, keep the evaluator trait and
ship the deterministic test harness while refining the model prompt. Do not let
the evaluator call tools to compensate; that would break the confirmed `/goal`
semantics.

### Phase 4: Subagent substrate and team evidence envelope

**Purpose:** Solve the biggest D4 composition hazard before building all 12 tools.

**Build:** Child sessions as supervised async actors, background task handles,
session ids, cancellation tokens, bounded queues, panic containment, and a minimal
team run with lead plus two fake members. Add the team evidence envelope and make
goal evaluation consume it through the normal transcript projection.

**Stub:** Full omo tool surface, category routing, skill injection, worktrees,
tmux, and real member providers.

**Validation gate:** A fake lead delegates to two child sessions. One succeeds,
one fails. The lead transcript receives a structured team evidence envelope, the
goal evaluator can judge it, and the server survives a panicking child.

**Rollback point:** If background supervision is unstable, pause all team-tool
breadth and fix the actor/supervisor model. Do not add worktrees or tmux until
force recovery is deterministic in-process.

### Phase 5: Full team-control-plane tools and lifecycle

**Purpose:** Implement omo parity in the control plane while execution is still
mostly in-process and observable.

**Build:** The 12 `team_*` tools, typed team state machine, in-memory mailbox,
task board, lead/member permissions, lead-only broadcast, member closure-ready
flow, graceful shutdown, reject shutdown, active-member delete rejection, and
force recovery. Store both background task id and child session id for every
member. Make all team tools use the same canonical tool wrapper and permission
engine as ordinary tools.

**Stub:** Real tmux panes and real worktree-per-agent can remain disabled behind
capability flags until Phase 7. Category routing can initially target deterministic
or cheap fake routes for repeatable tests.

**Validation gate:** State-machine tests cover valid and invalid transitions for
every tool. An integration fixture creates a team, assigns tasks, records member
messages, requests shutdown, approves/rejects, force-recovers one member, and
deletes the team without orphaned active members.

**Rollback point:** If the full 12-tool surface causes churn, keep the external
tool names fixed but implement rarely used commands as thin state-machine wrappers
over a smaller internal command set. Do not collapse shutdown into generic
messages; typed lifecycle is non-negotiable.

### Phase 6: Category routing, skills, and real provider members

**Purpose:** Turn the team control plane into a useful multi-agent system without
changing its state semantics.

**Build:** Category-to-model routing, fallback chains, route capability preflight,
member prompt/system-content construction, skill-content injection, member
question denial, per-category budgets, and mixed-provider child sessions.

**Stub:** If full dynamic skill loading is too risky, use static skill-content
injection and explicit category prompts in v0 while preserving the interface for
later true skill loading.

**Validation gate:** A team run launches at least three members with different
categories/routes, preserves context isolation, reports aggregate token usage,
and produces a lead-visible evidence envelope. Incompatible local routes are
rejected before launch.

**Rollback point:** If fallback chains are unstable, require explicit model routes
for v0 team specs and keep category defaults as configuration, not magic. Do not
route all categories through one global model override; that destroys the token
efficiency premise.

### Phase 7: SQLite and resume hardening under team load

**Purpose:** Make persistence reliable under the concurrency created by the core
differentiator.

**Build:** SQLite WAL setup, single writer actor, append-only event log,
projectors, session tree persistence, team metadata persistence, goal state
persistence, replay/recovery, queue-depth metrics, and lock-contention tests.

**Stub:** Remote multi-client beyond the local TUI can remain out of v0 polish.

**Validation gate:** A stress fixture runs many fake child sessions and provider
streams, kills/restarts the server mid-run, then resumes with correct session
tree, goal state, team status, message parts, and token ledger. No `database is
locked` errors are accepted.

**Rollback point:** If projections lag, keep event append durable and rebuild
projections on startup while optimizing projection writes. Never sacrifice event
durability for UI smoothness.

### Phase 8: Worktree and tmux integration

**Purpose:** Add the fragile external integrations after the server can already
supervise, cancel, persist, and recover teams.

**Build:** `WorkspaceRuntime` for git worktree creation/listing/cleanup,
per-agent working directories, ownership registry, dirty-state reporting,
force-recovery cleanup, and `PaneRuntime` for tmux pane launch/attach/status.
Team evidence envelopes must report worktree paths and dirty state.

**Stub:** If tmux is unavailable, expose a clear capability error and keep the
team run functional without panes only if the user explicitly selects that mode.

**Validation gate:** In a temporary git repo, create a team with worktree-per-agent
enabled, perform harmless file changes, show per-member dirty state, request
shutdown, reject deletion while active, force-recover a member, and clean only
owned resources. Tmux pane tests pass when tmux is installed.

**Rollback point:** If tmux is flaky, keep worktrees as the source of execution
isolation and gate tmux as an observability feature. If worktrees are flaky, do
not ship full D4 parity without explicit scope re-approval.

### Phase 9: Rich ratatui TUI and final v0 integration

**Purpose:** Productize the now-proven server semantics into a usable coding
agent interface.

**Build:** Ratatui client rendering normalized message/part lifecycle, streaming
assistant text, reasoning/tool sections, permission prompts, goal status, team
status, mailbox/task summaries, reconnect handling, token/cost display, and
worktree/tmux affordances. Keep rendering downstream of the same reducer used in
tests.

**Stub:** Theming beyond a basic palette, plugin systems, MCP, LSP, hosted mode,
desktop app, and web docs remain out of scope per D1.

**Validation gate:** Manual QA through the real TUI: start server, run a normal
single-agent tool turn, set a goal that loops once, create a team, watch member
status update, approve/deny a permission request, disconnect/reconnect the TUI,
resume the session, and verify the final transcript and token ledger match server
state.

**Rollback point:** If the rich TUI slips, preserve the server API and ship a
minimal but reliable TUI that exposes messages, tool parts, permission prompts,
goals, and team status. Do not trade correctness of stream/reconnect or
permission prompts for visual polish.

## Integration Seams That Must Stay Explicit

- `ProviderRoute -> CanonicalEventStream`: provider variance stops here.
- `CanonicalEventStream -> EventStore -> Projection`: persistence and UI use the
  same source of truth.
- `Projection -> ClientEventReducer`: TUI rendering never consumes raw provider
  chunks.
- `ToolDefinition -> ToolRuntimeWrapper`: schema, permission, execution, output
  validation, and transcript projection happen in one ordered path.
- `PermissionEngine -> ToolRuntime`: no side effect begins before permission is
  resolved.
- `SessionSupervisor -> SessionActor`: panic/cancel/backpressure containment is
  not a team-mode afterthought.
- `TeamControlPlane -> TeamEvidenceEnvelope -> TranscriptProjection`: this is the
  bridge between token-efficient team isolation and transcript-only goals.
- `GoalEvaluator -> TranscriptProjection`: evaluator independence is preserved by
  construction.
- `TokenLedger`: every lead, child, provider, evaluator, and summary cost lands
  in one accounting system.
- `WorkspaceRuntime` and `PaneRuntime`: git/tmux fragility is isolated from the
  orchestration core.

## Non-Negotiables

- Canonical provider event stream with conformance tests before serious feature
  work depends on real providers.
- Transcript projection and team evidence envelope before combining goal mode and
  team mode.
- Supervised child-session actors with cancellation, panic containment, and force
  recovery before real team breadth.
- Permission snapshots and scoped approval ids before concurrent subagents can
  run tools.
- Event ids, replay, and idempotent client reducers before claiming SSE/reconnect
  support.
- SQLite event durability and deterministic replay before final v0 acceptance.
- Hard caps for goals and team budgets before autonomous loops are enabled.

## Deferral and Cut Lines If Schedule Slips

These cuts preserve the confirmed product intent while reducing polish or breadth
around the edges:

- Defer theming, visual polish, and advanced TUI panes; keep reliable message,
  tool, permission, goal, and team views.
- Defer public remote server/SDK polish; keep the local HTTP/SSE boundary.
- Defer plugin/MCP/LSP/desktop/web docs; already outside D1.
- Defer file-backed multi-process team state; keep the trait and in-memory actor
  semantics for v0 unless multi-process becomes explicitly required.
- Defer true dynamic skill loading inside team members; use generated skill
  content and category prompts if the interface remains compatible.
- Restrict local/OpenAI-compatible routes by capability instead of promising all
  roles work on all local models.
- Treat tmux as the first candidate for explicit re-approval if schedule becomes
  impossible; worktree execution isolation matters more than pane observability.

Cuts that would violate the confirmed v0 and should require explicit user
re-approval:

- Removing multi-provider support rather than restricting unsupported route
  capabilities.
- Letting the goal evaluator call tools or inspect child transcripts.
- Shipping team mode without a lead-visible evidence envelope.
- Running subagents without scoped permissions and supervisor-controlled
  cancellation.
- Treating client/server as an implementation detail with no durable event/replay
  protocol.
- Dropping worktree-per-agent entirely from full omo parity.

## Recommended Sequencing Summary

The critical path is provider normalization first, transcript projection second,
goal evaluation third, supervised child sessions fourth, and full team breadth
only after those are proven. Worktrees, tmux, and rich TUI should come late because
they are fragile and visible, but they do not answer the core architecture
question. The first demoable loop should be a single session over local
HTTP/SSE: provider streams text, requests a tool, permission is checked, the tool
runs, events persist, the client renders, and replay/reconnect works. The second
demo should set a goal over that transcript. The third should add a fake team and
prove the goal evaluator can judge the team evidence envelope without seeing
child transcripts. Only then should yaca widen into the full 12-tool team surface,
real category routing, worktrees, tmux, and polished ratatui UX.
