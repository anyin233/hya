# Who keeps a supervisor alive, and what does shutdown join?

The built runtime owns it. Both engine seams are weak; the listener keeps a Weak
between deliveries and performs only synchronous indexed routing. Its explicit
cancel/JoinHandle remains separate from the task tracker that owns all episode,
main and per-team FIFO workers. Drain first requests and awaits episode endings,
then joins listener and task futures. A done watch can resolve before the task
drops its captured engine, so waiting only on watches cannot prove I19.

# Why protect retirement while envelopes are queued?

Terminal mail already holds routing_mail until bus delivery. FIFO routing also
counts queued/in-progress mail and recovery wakes under the team lock; otherwise the final episode
could retire main after enqueue but before its report was delivered. Killed
generations remain reachable through the existing episode index until their
tasks remove them. The current-root map and per-root driver state retain their
existing roles; replacing the latter with a team-local count would regress R4.

# How can a committed name reservation be released without allowing duplicates?

An owned HandleReservation lives with PreparedResident until registration commit
(or failed/cancelled preparation). Allocation reads durable names while holding
the same claim-write boundary as registration commit and reservation release.
Without that serialization, a concurrent allocator could read an old snapshot,
then mint after the committed reservation was released. Historical names remain
in durable roster/archive/inbox/channel identity facts, not a daemon-lifetime set.

# Can a late wait snapshot recreate a forgotten stall?

Recording holds the episode index while inserting the stall. Ending removes the
index entry before clearing stalls, so insertion either precedes cleanup or sees
that the episode is gone. Caller deletion clears its rows after store deletion.


# Why must Drop request endings before cancelling live teams?

Once the last supervisor Arc drops, a team's Weak cannot upgrade. Merely
cancelling its token leaves its task repeatedly asking the vanished supervisor
to kill it, without acquiring an ending or yielding. Drop now invokes the same
synchronous team ending transition used by explicit kill, before cancellation.
The takeover-before-first-turn test exposed the spin; a dedicated live-team Drop
test asserts the ending is installed synchronously and every task then exits.


# Which queued events can hold or retire a team?

Mail and lag recovery carry owed work. Generic activity only updates liveness.
Retiring after generic activity can discard an ensure_main context before its
first member registration, forcing root binding before the wake admits its
prompt. Both existing root binding-wake ownership tests reproduced that hang and
pass with mail/recovery-only queue liveness. Generic activity stays FIFO, but
does not delay retirement of a finished team or retire a prepared main.
