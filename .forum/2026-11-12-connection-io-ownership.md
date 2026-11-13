# Who completes plugin and MCP connection shutdown?

The existing connection owners retain their I/O handles. PluginClient owns
its stdout reader; the reader owns reverse host-capability handlers in a
JoinSet. PluginConn owns the event notification worker and the child guard;
its runtime source retains that same Arc. MCP clients own stdio/classic SSE
readers, and PreparedMcpServer retains its process guard. Streamable HTTP runs
inside the requesting caller rather than a detached I/O task.

Await the owner shutdown method for completion. Plugin stop_now signals and
kills synchronously but retains its handles, so subsequent awaited shutdown
can still join I/O and reap the child. Drop remains a nonblocking signal/reap
path and stops the connection before private directory cleanup.

# How does BuiltSessionEngine reach connections held by older bindings?

RuntimeSourceOwner now exposes async shutdown. RuntimeRegistry remembers
weak owner references at publication, prunes expired entries on later
publication, and joins live owners during shutdown_sources. It does not
extend the normal lifetime of a source. BuiltSessionEngine drains intake and
registration handlers, then residents, then these source connections. Bundle
owners delegate to their existing PluginHost/MCP server handles; materialized
files stay retained by the existing owner until its views are released.

The regression retains a binding, removes its configured plugin and MCP
sources from the active snapshot, awaits built shutdown, verifies both process
PIDs are reaped, and checks retained tools cannot reopen either connection.
Reader/handler/event/SSE tests also check task completion directly. This packet
does not own the resident bus listener or server-created tasks.

# What about initialization canceled before publication?

The same RuntimeTaskOwner has a connection I/O completion tracker. Preparation
carries it through `hya_mcp::io_tasks::scope`; connection guards capture it for
restarts and drop reapers. This is passive completion tracking, not another
connection/process owner. Built shutdown waits after source termination, so
aborted deferred preparation cannot leave an unjoined stdout/SSE task. The
regression gates classic SSE initialization before publication, verifies a
reader is tracked, then checks the tracker is empty and the peer observes EOF.
Runtime source shutdown terminates after resident drain, preserving its bounds;
explicit PluginHost/PreparedPlugin shutdown and declaration-drift rejection keep
their existing graceful handshake.

# Why does the stderr join use AbortOnDropHandle?

A descendant may inherit stderr after the direct child exits. The resident's
existing sidecar-stop deadline can cancel that join; a bare JoinHandle would
detach the reader indefinitely. AbortOnDropHandle stops it on cancellation,
and the same runtime completion tracker waits for actual exit. Normal stderr
tail collection and the sidecar deadline/diagnostic remain unchanged.
