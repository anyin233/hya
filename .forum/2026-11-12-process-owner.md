# What owns extension processes, and how do agent selections affect initialization?

Config plugins, Project plugins, explicit installed/Project bundle processes,
and private JavaScript activations normalize to `hya_plugin::config::PluginSpec`.
Its `ProcessLaunch` retains directory/environment policy and optional activation
metadata. `PluginConn`, through `PluginHost`, owns spawn, initialize, shared
connection restart, explicit shutdown/termination, and the child guard.

The audit's three-owner hypothesis was only partly true: config, Project and
explicit bundle processes already shared `PluginConn`. Startup/respawn branches
and the Project concurrent collector were duplicated. The separate owner was
the agent activation's raw client/guard, hook dispatcher and tool adapter.

An activation still loads only entrypoints selected by its captured binding.
Its initialize declarations must match **all declared exports of those loaded
modules**, not its agent's selected tool/hook subset. One module can export two
tools and serve two disjoint agent views. Nonempty process Skill declarations must match
the complete packaged Skill set; an empty list is valid, and static packaged
Skills never require a process declaration. No wire or manifest format changed.

Shared owners lazily restart with the same launch policy and full-declaration
drift checks. Private activation loss must reach the session supervisor;
transparent respawn would silently discard session-local JavaScript state.
Retained views cannot restart an explicitly stopped owner. Hot-reload source
retention, global hook order/event delivery, identity-based bundle shadowing,
and the native tool-family ABI loader remain separate existing invariants.

## How are concurrent failure and retained-view lifetimes bounded?

Failed calls invalidate only the client they used. Reconnection is serialized,
closed clients are replaced before dispatch, and stale errors from one crash
cannot kill its replacement or consume another restart. This is exercised with
eight in-flight calls to a controlled Python child, for both crash and timeout.

A private sidecar handle synchronously marks its connection stopped and takes
the child guard before removing activation files. This stop closes the client
and kills immediately, then reaps asynchronously; ordinary child-guard drop
retains graceful shutdown. Retained tool views
return Closed after handle drop or shutdown; resident.rs needs no explicit
shutdown call to make drop correct. Real engine-turn tests exercise disjoint
views for both a shared JS module and one explicit shared bundle process.
