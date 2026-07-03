# tmux-style TUI: single input to main, read-only observation panes

The multi-agent TUI is a tmux-style layout: an always-present, **uncloseable main-agent window**;
a bottom input bar whose keystrokes route **exclusively to the main agent**; and user-launchable
**read-only** panes/tabs for observing other agents live, plus roster and channel overlays. We
chose a single control channel (user ↔ main ↔ swarm) over letting the user type directly to any
focused subagent because it matches the actor model — residents wake only via mail (ADR-0002), so
there is exactly one place user intent enters the system. This keeps the bottom-bar model simple
and makes "which agent am I talking to?" un-ambiguous.

## Consequences

- The input invariant is enforced structurally: no code path (including opening a pane) ever points
  the input `route` at an aux session; pane focus only affects scroll/close/cycle. This is asserted
  by a harness test.
- Shipped as a **tab** model (focused pane full-frame + a tab bar), not side-by-side splits — it
  satisfies every hard requirement, degrades to any terminal size for free, and avoids refactoring
  the monolithic screen-draw signatures. True side-by-side split is a deferred follow-up.
- To redirect a subagent the user tells the *main* agent, which messages/re-tasks it — there is no
  direct-to-subagent input.
