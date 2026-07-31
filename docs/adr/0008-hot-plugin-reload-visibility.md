# Hot plugin reload visibility

Status: accepted

The `0.34.5` `RuntimeRegistry`/`TurnBinding` seam implements the required
atomic, next-turn visibility primitive for tools and deferred MCP results.
Plugin restart and desired/observed/effective reconciliation remain a
separate `0.34.6` responsibility; plugins loaded at startup are part of the
complete initial snapshot.

Hot plugin reload uses next-turn tool visibility: an in-flight Turn keeps the tool registry it started with, while the next admitted Turn resolves the current plugin and tool catalog. This keeps tool calls deterministic, matches hot skill reload visibility, and lets a running runtime pick up plugin changes without forcing a new Session.

Considered alternatives: new-session-only visibility would be simpler but would make runtime plugin reload less useful; immediate visibility would make mid-turn tool availability nondeterministic; configurable visibility would add policy surface before hya has evidence that multiple policies are needed.
