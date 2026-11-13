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

# What must M4-10 remove after moving loop continuations onto episodes?

The first loop round currently runs on a resident, then the controller ends
that episode before the transient executor reuses its Session. Otherwise the
transient registration overwrites its roster mode and prevents the resident's
normal ending from releasing its claim. Replace this temporary handoff with
repeated activations when moving loop workers and verifiers onto episodes.
W3's uncounted transient work and the transient deadline-notice selection
remain for that packet; neither executor nor selection was changed here.
