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
tools and serve two disjoint agent views. Packaged Skill validation also uses
the complete declaration. No wire or manifest format changed.

Shared owners lazily restart with the same launch policy and full-declaration
drift checks. Private activation loss must reach the session supervisor;
transparent respawn would silently discard session-local JavaScript state.
Retained views cannot restart an explicitly stopped owner. Hot-reload source
retention, global hook order/event delivery, identity-based bundle shadowing,
and the native tool-family ABI loader remain separate existing invariants.
