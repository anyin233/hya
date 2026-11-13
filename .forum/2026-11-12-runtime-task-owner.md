# Who owns composition tasks and how must an embedder stop them?

`BuiltSessionEngine` retains one `RuntimeTaskOwner` (a cancellation token and
`TaskTracker`). Spawn intake and registration handlers, Workflow tool requests,
Started Workflow runs, deferred MCP preparation, mailbox/lifecycle services,
and scope sweeping belong to it. Mailbox/lifecycle services each own and join
their request `JoinSet`; deferred MCP cancellation aborts and joins preparation
children. Workflow admission itself holds a tracker token, so shutdown cannot
finish while an in-flight direct request can still start a background run.
The same token-before-cancellation check now guards public catalog listing and
state decoration. Catalog builds use tracked blocking tasks. Tool request
admission is bounded by the existing channel capacity; a Finished run releases
its request slot at durable admission, preserving the 250 ms List/State test.

Await `built.shutdown()`; drop signals cancellation without waiting.
`BuiltSessionEngine::drain(cause)` owns the stop order for serve, headless
commands, and restart: close the turn gate, cancel/join composition handlers,
then drain residents. `shutdown()` delegates to it. Callers must not drain
the resident supervisor ahead of this ordering. A regression gates a registration
until stop and checks that the first drain joins it; shutdown leaves no active
claim. The lower-level `spawn_team_supervisor` returns an owner too: retain it across fixture/function returns, then await its
`shutdown()`. Dropping a local owner inside a fixture constructor closes the
service before the returned engine can use it.

## Does this prove every task reachable from construction is gone?

Yes after explicit shutdown: the supervisor now joins its listener and tracks
every main, episode and team queue task, including killed generations. Its
engine seam is weak and the listener holds only a weak supervisor between
deliveries. Episode completion is not itself a task join. Plugin/MCP connection
I/O joins through the existing io_tasks tracker after sources stop. Dropping
the runtime signals cancellation; callers still await shutdown for completion.

`HyaRuntime::start` additionally constructs the server router. Server services
have their own owner (see below), separate from `BuiltSessionEngine`. Await
`HyaRuntime::shutdown()` to join both; shutting down only a built engine does
not stop a separately constructed server state.

## Which review hypotheses reproduced?

F2's premature caller drain reproduced as a handler still live when the first
drain returned. Its claimed killed-team claim leak did not reproduce on
`d525b646`: `team_for` already waits for killed slots and replaces the team.
The caller-order fix does not change that resident machinery. F3 reproduced
both runtime-thread catalog work and acceptance beyond configured capacity.
F4's stated execute-token race was absent (token already precedes cancellation
check), but public `list` and `decorate` lacked that gate; post-shutdown listing
failed the added regression before the fix. F1 cancellation is runtime-shutdown
only; live reconciliation callers use fresh uncancelled tokens, so no change.

## Who joins server services?

`AppState::shutdown().await` cancels and joins the server's frame/catalog relays,
Project busy watcher, session-list tracker, ephemeral startup sweep, delayed
reaper children, and permission/question bridges. The join comes last:
`stop_for_restart` and `HyaRuntime::shutdown()` fence admission and close streams,
drain turns, Workflow runs and residents (`built.shutdown()`), and only then join
these services, so a turn waiting on a permission or question is cancelled by the
stop instead of seeing an unavailable bridge.
`RuntimeTaskOwner` is now defined in hya-server and re-exported through the
existing hya-app path. Routers retain the owner in an extension; service tasks
hold only token/tracker handles, avoiding an owner/task cycle. The resident listener and team tasks join during resident drain; plugin/MCP
connection I/O joins through the owner's `io_tasks` tracker after sources stop.

## Who joins I/O for the standalone spawn supervisor?

`spawn_team_supervisor` explicitly puts its returned owner's I/O tracker in
the sidecar environment. Its owner tracks an ordered cleanup task: cancel/join
the inner intake owner, drain residents, stop runtime sources, then close and
join I/O. The shared `RuntimeTaskOwner::shutdown` still joins only its service
tasks: making every owner join I/O there would deadlock BuiltSessionEngine,
which stops sources after joining composition handlers. AppState has no
connection I/O. The standalone regression keeps a connection task pending and
checks shutdown cannot complete before that task exits.
