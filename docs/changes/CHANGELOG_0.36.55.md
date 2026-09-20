# 0.36.55

## System-prompt tool guidance realigned with the current registry

The team quick reference appended to every system prompt still taught
the removed `dm`/`broadcast` tools. Its mail guidance now describes the
unified `send` semantics (`#channel` posts, bare handles DM vertical
peers with `^parent` and archived-child revival, omitted channel uses
the role default), and the archived-agent revival hint names `send`.

A new invariant test guards the alignment going forward: every
tool-shaped backtick token in the quick reference must resolve in the
current builtin registry, so a future tool rename or removal that
leaves prompt guidance behind fails CI instead of teaching models
tools that no longer exist.
