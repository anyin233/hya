# Who owns composition tasks and how must an embedder stop them?

`BuiltSessionEngine` retains one `RuntimeTaskOwner` (a cancellation token and
`TaskTracker`). Spawn intake and registration handlers, Workflow tool requests,
Started Workflow runs, deferred MCP preparation, mailbox/lifecycle services,
and scope sweeping belong to it. Mailbox/lifecycle services each own and join
their request `JoinSet`; deferred MCP cancellation aborts and joins preparation
children. Workflow admission itself holds a tracker token, so shutdown cannot
finish while an in-flight direct request can still start a background run.

Await `built.shutdown()`; drop signals cancellation without waiting. Shutdown
settles registration handlers before draining residents, so a late registration
cannot start after the drain. The lower-level `spawn_team_supervisor` returns
an owner too: retain it across fixture/function returns, then await its
`shutdown()`. Dropping a local owner inside a fixture constructor closes the
service before the returned engine can use it.

## Does this prove every task reachable from construction is gone?

No. At this cleanup step's base (`95cbfa50357119960497f99e07904ac7cf205fae`),
`ResidentSupervisor` exposes `drain` but no bus-listener/task join seam;
`resident.rs` is reserved for the concurrent resident lifecycle executor.
`hya-plugin/src/client.rs` and `hya-mcp/src/{client,http}.rs` also detach
connection I/O tasks and expose no complete transitive join seam to the built
runtime. Composition tracker emptiness proves only the tracked composition
subtree has exited. Follow-up must connect these owners before claiming the
stronger transitive guarantee; neither sleeping nor an empty tracker proves
unregistered tasks have stopped.

`HyaRuntime::start` additionally constructs the server router. Its server-owned
catalog notice relay, Project busy watcher, session-list tracker, and ephemeral
reaper are started by `AppState::server_state` and do not expose join handles
to `BuiltSessionEngine`. Wrapper-level shutdown needs that owner as well.
