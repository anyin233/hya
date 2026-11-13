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

# Why is deferring recovered installation insufficient by itself?

The server previously acknowledged restore admission before core acquired the
root turn lease (external-turn validation can await). A recovered child's mail
could win that gap. Restore admission now reserves the lease first and executes
through run_admitted_turn, which consumes it. The app startup helper is shared by
the daemon, HyaRuntime and I22; construction stages actors, the helper admits all
restores, then start_recovered installs actors and queues root inbox recovery.
The old I22 fixture only checked its first provider request and could miss
stranded mail. It now gates the restore answer and requires the persisted mail's
distinct main prompt afterward, with exactly one answer for the restore.


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


# What if a restore tool ends an actor before recovered installation starts?

That actor is still an orphan between construction and start_recovered. Its
normal end_member Takeover can commit during restore admission. Installing the
cached old claim then returns StaleActorClaim. Startup skips exactly that typed
fencing result, preserves the archive/successor, and continues its peers; every
other error propagates. The returned count includes only installed episodes.
The real crash/end_member regression failed before this change with
StaleActorClaim and checks that the peer starts while the ended actor stays archived.
