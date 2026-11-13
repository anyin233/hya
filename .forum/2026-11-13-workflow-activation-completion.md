# What completes a Workflow Stage activation?

`ResidentSupervisor::activate` installs guidance, route and an inbox-boundary
completion on the episode, then delivers durable parent mail. The existing
turn lease prevents any wake from starting before installation completes.
The episode resolves `ActivationWaiter` after durable settlement, a terminal
report, or a forced ending. The Stage controller accepts both settlement and
Done reports; a report accepted during the turn takes precedence over settlement.
Request-local route recorders are dropped before completion, even though a
settled actor remains live. Dropping a pending wait does not end its episode.

There is no Stage bus subscription or per-envelope Projection poll. The
subscription regression counts the ordinary turn mail-steering listener too:
a single active Stage has two subscribers (supervisor + steer), versus three
before the activation API. Use a valid `hya/offline` route when measuring it.

# What cleans up a partially registered Workflow level?

Retain each committed registration receipt before linking or registering the
next actor. Any unsuccessful run exit awaits every run-owned actor's normal
ending while still holding `RootDriverActivity`. Concurrent task actors are
untouched. A graph larger than the configured budget is invalid source;
current occupancy and transactional registration/revival capacity refusals
map to `WORKFLOW_OVERLOADED` (tool Overloaded, HTTP 503, gRPC ResourceExhausted).

# How was the temporary first-loop handoff removed?

M4-10 replaced transient continuations with repeated resident activations.
The first settled episode remains live; a reported worker is revived under its
original identity. Verifiers are fresh resident episodes and release their
capacity after each judgment. See [the iteration Q&A](2026-11-13-workflow-resident-iterations.md).
