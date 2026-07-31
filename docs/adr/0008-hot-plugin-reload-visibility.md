# Plugin declaration refresh visibility

Status: amended

The `0.34.5` `RuntimeRegistry`/`TurnBinding` seam implements the required
atomic, next-turn visibility primitive for tools and deferred MCP results.
Release `0.34.6` adds desired/observed/effective reconciliation for MCP and
stable source ownership for startup plugin tools. It also validates the full
initialize declaration after a plugin crash/respawn. A drifted respawn is
closed and calls fail closed; it is not activated as a new declaration.

Any future explicitly supported plugin declaration replacement uses next-turn
tool visibility: an in-flight turn keeps its retained source owner and tool
view, while the next admitted turn resolves a successfully published complete
candidate. This keeps dispatch deterministic and matches skill visibility.

This ADR does not claim that `0.34.6` implements plugin watching, hot add/remove,
or a reload command. Hooks, commands, and permission callbacks remain on the
existing `PluginHost` lifecycle rather than entering a new dynamic control
plane. Immediate mid-turn replacement remains rejected; configurable
visibility would add policy surface without evidence.
