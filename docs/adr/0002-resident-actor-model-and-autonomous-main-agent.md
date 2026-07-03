# Resident actor model and autonomous main agent

Subagents have a **hybrid** lifecycle: `transient` (the default — spawn, run one turn, return a
bounded summary, parent blocks) stays unchanged, and a new opt-in `resident` mode makes a subagent
a long-lived, event-driven **actor** that idles at zero token cost and wakes on inbound mail to run
exactly one turn. The **main agent is also an actor**, woken by child mail, so a team runs
hands-free to completion. We chose event-driven wake over a polling goal-loop because 100+ agents
each looping would burn tokens continuously and need a per-agent stop condition; event-driven
actors cost nothing while idle and reach a natural **quiescence** (all idle + no mail in flight)
that wakes the main agent to synthesize.

## Consequences

- Because an idle actor swarm could deadlock (everyone waiting) or run away (agents messaging
  forever), the design carries two guards that are *not* optional: a race-free quiescence detector
  (the fire decision happens in the same locked section that observes no pending work, with a
  `work_seq` termination guard so a no-new-work synthesis doesn't re-fire) and per-team
  turn/message budgets that cancel a runaway team.
- Resident spawns are **non-blocking** (parent gets the handle and continues), which diverges from
  the transient `run_team` join model — the two spawn paths coexist.
- User input never wakes a resident: residents wake *only* via mail. This is what lets the TUI bind
  all user input to the main agent (see ADR-0003).
