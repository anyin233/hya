# Task Tool Contract

## Scenario: Single And Batch Input Validation

### 1. Scope / Trigger

- Trigger: changes to the `task` tool schema, resume IDs, member normalization,
  or `SpawnerPlane` dispatch.

### 2. Signatures and validation

- Preferred input: `{context?: string, tasks: TaskMember[]}`. `tasks` must be
  non-empty; `members` is a compatibility alias and cannot coexist with it.
- Each task requires a non-blank `prompt`; optional `description`,
  `subagent_type`, `category`, `model`, `effort` and `inline_agent` belong to
  that task. Empty or omitted agent selects `hya-task`.
- Legacy single mode requires top-level non-blank `description` and `prompt`.
  Batch mode does not require or consume those single-task fields.
- Non-blank `context` is prepended to every normalized prompt with two newlines.
- `name` remains rejected (top-level or member); runtime derives handles from
  the resolved agent type. `task_id`, `background`, `resident` are obsolete;
  every spawn is non-blocking and follow-up uses mail.
- Validate all prompts and all permissions before one `SpawnerPlane` request.
  Runtime roster/model preflight and admission govern the entire batch.
- Runtime resolves the whole batch, registers all members in one store
  transaction, then installs every episode before awaiting initialization.
  Batch response stays a batch even for one task; registered members are running.
  Any registration failure rolls back all siblings and lazy root registration.

### 3. Verification

`cargo test -p hya-tool --test task` covers schema, context propagation, input
validation, legacy inputs, permission and typed errors, and result shapes.
`hya-app/tests/spawn_admission.rs` proves multiple children enter provider work
before either is released. Track P `p08_subagent_task` proves the batched tool
creates distinct child sessions and produces independent child outputs.

## Scenario: Atomic Spawn Admission

Resolve all members and root context before effects. `register_episodes` fences
the parent and checks active claims under one SQLite writer reservation, then
commits the complete batch. `per_run_budget` limits live members per root, across
turns and restart. Archive frees a claim; revival consumes one. Root turns never
reset capacity. See [the exact store contracts and worked example](../../architecture/admission-and-governor.md).

The bounded `SpawnerPlane` transport still fails fast with `Overloaded` when
full and `Unavailable` when closed. Its capacity derives from `per_run_budget`,
clamped to `1..=tokio::sync::Semaphore::MAX_PERMITS`. Capacity/depth rejection
creates no child, roster entry, or provider call. `ToolOperation` retains source
call and actor identity, but task dispatch has no journal or replay API.

Regression coverage: `hya-app/tests/resident_lifecycle.rs` I13/I14 exercise real
task/archive/mail and process crash; `spawn_admission.rs` covers batch rollback,
intake pressure, authorization and concurrent child initialization. Store
registration tests check rollback of the entire batch. Tests that require an
inactive lead hold a real root turn lease with `engine.try_begin_turn(root)`.

## Supervisor shutdown

### 5. Invariant: Workflow-intake shutdown

The Workflow host has one worker and at most one in-flight
`handle_workflow_request`. Lifecycle shutdown must select the shared supervisor
stop token against that execution. When stop wins, it must:

1. cancel the `WorkflowRequest.cancel` token;
2. keep awaiting the governed request so member/session cleanup finishes; and
3. exit the worker before `SpawnSupervisorLifecycle::shutdown` returns.

Do not only watch `stop` around `rx.recv()`: once a request is dequeued, a
provider or independent verifier can remain pending indefinitely. Do not merely
drop `handle_workflow_request`, either: governed members run in spawned tasks and
need the cooperative token to reach their Turn cancellation boundary.

Behavior contract:
`runtime::tests::workflow_supervisor_stop_drains_an_in_flight_run` parks a real
Workflow member inside the provider, cancels lifecycle stop, and requires both
the supervisor and tool reply to become terminal. Replacing the in-flight
`select!` with a plain `.await` must make this regression time out.

## Scenario: Atomic Resident Registration and Arming

A successful `task` batch commits every child `SessionCreated`, parent's
`MemberSpawned`, root roster/channel events, actor claim and optional directive
`MailSent` in one SQLite transaction. The mail is from the direct parent on the
new parent–child DM channel. For example, spawning a worker with prompt
`inspect the parser` persists that DM before spawn returns; takeover before its
first wake sees ordinary queued mail. Parked Workflow registrations omit mail
and await their first activation.

After commit, registration installs every episode under claim-write serialization
without awaiting a plugin. Each episode sends global and scoped `session.start`,
publishes registration envelopes, notifies the parent's `agent.spawn` hook, then
starts its resident loop. The first wake coalesces the directive and any later
mail as `[mail from main] inspect the parser`, runs `message.user.before` once
per hook on that whole text outside SQL and locks, then calls `begin_work` to
admit one User message and work boundary in one transaction. Cancellation races
the hook; an unadmitted directive stays unread. Direct-parent DM mail is always
deliverable even when bundle policy restricts `resident_mail` or `follow_up`.
Dropping the spawning future during a hook does not drop the episode task.
A failed registration transaction creates no hook/policy entry or child callback.
After commit and episode installation, the batch always returns every committed
identity. A member's initialization failure belongs to that episode's ending and
parent report; it cannot make `task` report that the whole batch failed.

`SessionStore::begin_work(claim: Option<&ActorClaim>, root: SessionId,
handle: &str, inbox_through: u64, prompt: ResidentWorkPrompt,
current_task: String) -> Result<(MessageId, Vec<Envelope>), StoreError>` accepts
`Text(String)` for new mail or `Resume { message: MessageId, append: String }`
for admitted work retained after user cancellation. Fresh mail needs no child
projection read inside the wake transaction. Resume reuses an unanswered prompt;
an answered prompt remains immutable and new work is admitted at the tail.

Each slot belongs to its exact `ActorClaim`, including epoch and owner. After
an awaited hook or other asynchronous operation, initialization and the resident
driver recheck ownership before publishing, replacing session hook/policy maps,
updating slot state, or entering another turn. If a parent archives and revives
the child while an old `session.start` or `agent.spawn` is blocked, releasing
that hook silently retires the old task; only the revived episode drives the
session. Map replacement and its ownership check share the team lock.
`take_episode(session: SessionId, claim: Option<&ActorClaim>)` removes only a
matching episode and returns its completion sender; the ending publishes its
envelopes before resolving that sender. `None` belongs only to the main actor.
A claim stays fixed for the whole in-memory episode. Resident recovery runs only
after a process restart; an orphan may still be ended using `Takeover`.

Concurrent sibling registrations resolve their unit's group channel under the
same SQLite writer reservation. A single `announce` therefore reaches every
registered sibling. For example, a batch of eight first children joins one
`main` group, with one persistent DM channel per child.

If directive mail is archived, stopped, fails sidecar startup, or is killed by a
team budget before wake admission, it stays unread and appears in the handoff's
Goal and Pending sections. No child user message or synthetic cancellation answer
exists. For example, archive `inspect the parser` before its first wake, then
revive with `inspect the tests instead`: only the new mail is admitted, alongside
the prior handoff recording the unexecuted directive.

Store contract: [`register_episodes`](../../architecture/admission-and-governor.md#store-interface)
returns one claim and committed envelope group per member, in input order. A
supplied parent claim is fenced inside the same transaction. Publication uses
only those committed envelopes, once per episode, after its session-start hooks.

`registered_initial_task_survives_claim_takeover_before_first_turn` covers recovery after
registration and before the first worker turn. This does not promise replay of
work that already crossed `ResidentWorkStarted`. The
`engine::session_hooks::registration_tests` regressions cover stale-parent and
database rollback, hook ordering, dropped spawns, concurrent announcements, and
all pre-first-turn stop paths including archive followed by revival. The two
`revived_resident_ignores_stale_*` tests hold the old initialization in each
lifecycle hook while the revived episode is Busy, checking publication, hook
map ownership, Busy/Idle activity and subsequent stop cleanup.

### Live provider routing

Each spawn admission captures the engine's current provider router once for
model preference and category resolution. Adding or refreshing a provider needs
no supervisor restart. For example, after adding a provider and saving its model
as an Agent default, the next `task` request for that Agent uses that model.
The `task` request schema and response handles above are unchanged.

## Atomic Resident Termination

Resident report, stop, turn error, budget kill and sidecar startup failure use
one store terminal transition. It fences the claim, checks the handoff generation
and expected resident cursor/work, and for voluntary reports rechecks unread mail
and live children under the SQLite writer reservation. Mail arriving during
handoff generation refuses the report archive. The refusing transaction commits
only a direct `MailSent` from `harness` to the member, explaining the unread
mail, live subagent, or changed episode. Publication wakes the member; it remains
live and must read the notice and handle the blocking work before reporting
again. The refused report is discarded: handling the mail never re-submits its
old outcome or text.

`finalize_resident_archive(owner: ResidentArchiveOwner<'_>, root: SessionId,
handle: &str, archive: ResidentArchive)` returns
`Result<(ResidentArchiveOutcome, Vec<Envelope>), StoreError>`.
`ResidentArchiveOwner::Claim(&ActorClaim)` fences the episode's capability;
`Takeover` ends an orphan whose caller guarantees has no in-memory episode.
Takeover increments the active claim's epoch, replaces its owner and releases
the claim in this same transaction.
A failed transaction rolls back the takeover too.
`ResidentArchiveOutcome::Committed` carries terminal effects (or empty effects
for a matching repeated finalization). `Refused` carries only the harness notice. A store error is a failed write, not a refusal.
`ResidentArchive` carries `actor: SessionId`, `generation: u32`,
`doc: String`, `degraded: bool`, `reason: ArchiveReason`, `note: String`,
`member_status: MemberRunStatus`,
`report: Option<(ReportOutcome, String)>`, `check_gate: bool`, and
`expected_work: Option<(u64, Option<ResidentWorkProjection>)>`. Handoff, optional
parent report/mail, cancellation cleanup, terminal activity, archive marker and
claim release commit together. `publish_resident_finalization` publishes only
after commit. Stop/failure entry points choose terminal reasons and
use this same transaction; unadmitted directive mail stays pending in the handoff. The
caller supplies the archive reason, member status and handoff document; the
store never synthesizes a handoff or derives a reason from the member status.
Forced endings use the shared projection-derived handoff template, retaining a
previous handoff's document when one exists. Its numbered goal and pending-task
sections remain searchable through `search_agent`.

Parent, user and Workflow archives record `archived_by_parent` / `cancelled`
without report mail. Turn errors and sidecar startup failures record `reported`
with a failed parent report and DM mail. Budget kill, root teardown and graceful
shutdown retain their explicit reasons. `wait`
returns the current episode's terminal member status and reason, including a
failure before a revived member's first turn; it never returns an older report
for a forced ending. A failed terminal transaction rolls back every effect and
is returned or logged with the member session id. Shutdown has no fallback
terminal write path.

The `report` tool accepts `{result: string, outcome?: "done" | "failed"}`;
`result` must be nonempty and `outcome` defaults to `"done"`. For example,
`report({"result":"Updated the parser and verified its tests","outcome":"done"})`
returns `title: "Report submitted"` and an `output` beginning
`Report submitted; it takes effect when this turn ends.` Submission closes the
turn after its current tool round. If the terminal recheck refuses it, the next
wake includes `[mail from harness] Your report was not accepted: <reason>. Read
and answer it, then call report again.` The report takes effect only when its
archive commits. Terminal activity never consumes unread mail based on the
terminal note; only explicit delivery/consumption advances the cursor.

For example, after a budget kill, send a new directive to the archived child to
revive it through the existing mail revival path. A killed team leaves the live
map immediately. A new `task` uses fresh turn/message counters and the new
triggering turn's `main` guidance; it does not wait for unrelated endings.
Revival waits only for the same member's previous episode completion.

The supervisor serializes claim commit plus episode installation with orphan
Takeover across team generations. Kill itself never waits on registration or
plugin hooks. Registration committed during a kill installs an already-ending
episode. Its registration envelopes publish before its archive envelopes; if
initialization is cancelled, the spawn returns `CoreError::Cancelled`, which the
`task` reply represents as a failed member rather than `status: "running"`.
Revival's prior-handoff message still commits with its new claim and arming
directive; bounded episode context is separate work.

`SessionEngine::force_archive_team(root: SessionId) -> Result<(), CoreError>`
requests `RootTeardown` endings for all live episodes, awaits their completion,
then reconciles remaining roster orphans with Takeover. It does not acquire a
member's turn lease or mail failure reports to parents. A root delete waits for
this boundary before deleting its log, and propagates a terminal write failure.
`ResidentSupervisor::drain(cause: FinishCause, deadline: Duration) -> TurnDrainReport`
refuses new turns, drains active turns alongside `Shutdown` endings, awaits all
episodes, reconciles actors already orphaned when draining began, and cancels and
joins the bus listener. A terminal write failure during this drain leaves an
orphan for the next ending request. Runtime
intake and registration handlers must already have been joined before calling
this lower-level drain. An ending that cannot stop its sidecar within five
seconds drops the handle and still removes the episode; a forced ending's member
summary includes `sidecar did not stop: ...`.

Budget endings use `AgentArchived { reason: Killed }` and
`MemberFinished { status: Failed }`; teardown uses `RootTeardown` / `Cancelled`;
shutdown uses `Shutdown` / `Cancelled`. None sends report mail. The first ending
request wins. Cancelling the public team token requests shutdown even while an
episode is idle or generating a report handoff. Terminal envelopes are published
after removing the episode from memory, before resolving its completion; the
next episode cannot publish a new registration ahead of those envelopes.

### Atomic mail wakes

A harness wake delivers all eligible unread mail in one user message, with
`[mail from <sender>] <body>` sections separated by blank lines. For example,
sending two messages while a worker is busy produces one follow-up turn and one
prompt containing both messages. Main mail wakes use the same path; a quiescence
wake includes `TEAM QUIESCED` in its owned user prompt. No new tool, flag or
configuration is required.

`SessionStore::begin_work(claim: Option<&ActorClaim>, root: SessionId,
handle: &str, inbox_through: u64, prompt: ResidentWorkPrompt,
current_task: String) -> Result<(MessageId, Vec<Envelope>), StoreError>` holds
one `BEGIN IMMEDIATE` transaction. `Some(claim)` fences the exact episode and
writes `ResidentWorkStarted { session: root, actor_session, handle, epoch,
inbox_through }`; its cursor advances when activity returns to Idle. `None`
is the root lead and writes `MailConsumed { session: root, handle, through:
inbox_through }`. Both write `AgentActivityChanged { status: Busy,
current_task }` and the prompt together. Publication occurs only after commit.
A stale claim or any failed write rolls back delivery and consumption together.

`ResidentWorkPrompt::Text(String)` admits a new User message with its text part
and finish. `Resume { message: MessageId, append: String }` resumes an admitted
wake retained by user cancellation. It reuses the unanswered prompt and appends
new mail with `TextReplace`; after an answer it admits a new prompt at the tail. Every harness turn passes the returned
message id as `TurnRequestContext.admitted_prompt`, including lead and synthesis
turns, so cancellation during binding closes the owned answer and shutdown can
record the root's resume obligation.

Global and bundle user-text hooks run once on coalesced mail/synthesis text
before the transaction, outside locks, including the first directive DM.
Cancellation or failure before the work commit leaves the mail unread. Policy-ineligible mail is consumed without
starting a turn or charging the turn budget, including multiple queued messages.

### Mail during a running turn

Unread team mail can steer an active turn without starting another turn. For
example, a peer sending “focus on parsing” while a tool runs causes the next
successful tool result to carry a `[NEW MAIL]` notice. Channel steer permissions
still apply; mail after an accepted report remains unread for the next wake.

The result and `MailConsumed { session: <team root>, handle: string, through: u64 }`
commit in one store transaction under the turn's actor claim, when present.
Observers are notified only after commit. Failure to append either event rolls
back both. Object results append the notice to a string `output` field, or add a
string `steer` field. Other JSON results become `{ "result": <original JSON>,
"steer": <notice string> }`, preserving the tool value and the notice. Results
without a notice keep their original JSON shape. No new command or configuration
is required.

### Episode completion and client turns

The episode task performs report, turn/start failure and requested archive
endings; requesters only set the first ending and await its completion.
`ResidentSupervisor::end_member(root: SessionId, handle: &str) -> Result<(), CoreError>`
is the Workflow cancellation entry. Parent/user archive retain the tool and API
schemas above. A dropped caller never cancels the ending. A failed terminal write
removes the episode, returns its error to archive waiters and leaves an orphan;
a later archive reconciles it with Takeover under the supervisor claim-write boundary.

Direct user prompts and turns on any registered team member, live or archived,
return `CoreError::Invalid` telling the caller to steer the subagent with mail.
For example, send new work
with `send({"channel":"main/hya-task-scout","body":"review the tests"})` instead of
starting a turn on its child session id. Model/agent switches and revert/unrevert
are refused too; forks remain independent roots and leave the member unchanged.
HTTP refusals are `400 invalid_argument`; gRPC returns `INVALID_ARGUMENT`, both
with the mail guidance. Root/main sessions, never-registered sessions, restore
prompts and internal bound Workflow entry points remain allowed. See
[the client mutation contract](../../architecture/subagent-orchestration.md)
for the admission predicate and API routes. Empty or duplicate mail notifications
never cause a provider request without a directive, unread mail or synthesis.

### Sidecar loss and bounded turn ownership

A resident turn uses a child of its episode's cancellation token. Losing a
sidecar during a turn requests `Failed("activation sidecar lost")`, cancels the
turn, and keeps polling it so its text and tool cancellation results flush before
the terminal transaction. The episode reports failure to its parent through
`SubagentReported { outcome: failed }` and DM mail, archives with reason
`reported`, and releases its unchanged claim. It never recovers that claim in
process. For example, after such a failure the parent sends new mail to the
member's handle to start a new episode under the next claim epoch.

Idle sidecar loss only discards that handle; it sends no report and keeps the
claim. The next mail wake starts a fresh sidecar. Every sidecar termination is
bounded by the internal `SIDECAR_STOP_DEADLINE` (five seconds), including idle
loss and unhealthy-handle replacement.

After cancellation, the episode awaits its own turn for `DRAIN_DEADLINE` (five
seconds). A cooperative turn completes normally through cancellation. A
straggler's message is closed under that episode's claim before its future is
dropped; late tool results cannot append after the close. Ending writes begin
after the turn is gone, so they cannot wait for a pooled connection retained by
the suspended turn. Sidecar stop errors or timeouts append `sidecar did not
stop: ...` to the ending note. The episode is removed before archive envelopes
are published and before completion is resolved. No tool or wire schema changes.

Loss only fails an episode when it interrupted a turn that ended cancelled.
`run_one_turn` distinguishes `NotStarted`, `Finished`, and `Cancelled`; a sidecar
lost after a completed Stop/Length turn or during an empty wake is discarded
without replacing an accepted report or archiving the member. For example,
`report({"result":"done"})` followed by loss after its completed turn still
reports Done. Loss during that tool round instead cancels it and reports Failed.
The loss token cancels the owned turn first; after draining the turn and stopping
the handle, the task chooses its ending. An archive, teardown, kill or shutdown
requested during those waits wins over the provisional sidecar failure.

The five-second straggler boundary closes main's assistant message without a
claim, and member messages under their claim, before dropping either future.
`SidecarHandle::is_healthy() -> bool` defaults to false when its optional
`loss_token() -> Option<CancellationToken>` has fired. Cached handles follow
that same rule before another turn, including before the idle-loss branch runs.
Terminal report mail holds the team against retirement until the bus listener
routes it (or recovers its durable inbox after lag); archive envelopes still
publish after the episode is removed and before its completion is resolved.

### Creation hooks and revival boundaries

Initialization does not process prompt text. Both creation directives and
reviving mail use ordinary wake admission. The System handoff boundary is never
passed to user text hooks or rewritten, so its context marker keeps its prefix
and the next request includes only the latest handoff and current episode.

`agent.spawn` describes member creation and runs once for each registered hook
when `SessionCreated` belongs to the initializer. Revival reattaches session
hooks but does not emit another creation callback. For example, task → archive →
mail runs one member creation callback and two episodes.

`SessionEngine::with_base_system_prompt(prompt: String) -> Self` retains the
host's base prompt for reconstructing resident and autonomous main agents. App
composition supplies its host `AgentSpec.system_prompt`; core embedders with a
nonempty host prompt supply it through this builder. A catalog `prompt: None`
keeps that base after revival, while `Some(prompt)` still replaces it.

### Supervisor lifetime and event routing

The built engine owns the resident supervisor; the engine's revival seam is
weak, as is its turn observer. `BuiltSessionEngine::shutdown()` fences admission,
joins composition handlers and Workflows, drains turns and resident endings,
then joins the supervisor listener and all team tasks before stopping sources
and joining connection I/O. Server-owned services join last. Dropping the owner
signals cancellation; embedders await shutdown to prove task completion.

The single bus listener performs only indexed routing. Member session IDs map
to their owning episode's team, including killed generations; root IDs map to
the current team. Each team has a FIFO queue for mail, activity and lag recovery.
It processes its own queue outside locks, so a slow team cannot delay another
root. Queued mail/recovery wakes and unpublished terminal mail prevent idle retirement.
Generic activity does not retire a main context prepared for registration.

Name reservations cover only uncommitted resident preparations. Registration
commit releases them before initializer hooks; dropping a failed or cancelled
preparation releases them too. Allocation and commit share claim-write
serialization so a stale pre-commit snapshot cannot reuse a released name.
Durable roster, archived identities, inbox owners and channel members preserve
historical uniqueness. No naming options or wire fields change.

Wait-stall deduplication lasts only for the member's episode and caller lifetime.
Ending removes the member's entries, session deletion removes that caller's
entries, and a late wait snapshot cannot recreate an ended member's entry.
