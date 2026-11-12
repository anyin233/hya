# Why does public deletion refuse an individual team member?

A member's registration remains its durable identity after archive. The existing
supervisor archive methods end episodes, but leave the handle revivable, and
expose no boundary that fences revival/registration against subtree row deletion.
That boundary is private to `resident.rs`. While its lifecycle run section is
being changed independently, public and engine deletion refuse live and archived
registered members with `invalid_argument`: archive the subagent or delete the
root. Unregistered child rows remain deletable. This is the coordinator-approved
D1 rejection alternative, not support for partial team deletion.

# What settles before public root deletion removes its rows?

The shared HTTP/gRPC path fences server admission for the current subtree,
cancels server runs, engine turns and Workflow execution, waits for server run
guards, then cancels and joins the app-owned Workflow run again (covering a
Started handoff across the first cancellation). Engine turn leases must release;
closing their messages alone is insufficient. Automatic titling is owned by its
initiating server run too. Only then does engine teardown await resident endings
and reconcile orphan claims, before store deletion. The existing drain deadline
bounds writer settling and resident teardown; a timeout leaves rows intact.
Deletion closes an existing server run's pending admission even after that timeout;
ordinary `CancelTurn` still admits and answers its prompt with `UserCancel`.
Restart restore admission continues to skip automatic titling.

The ephemeral reaper still uses its independent quiet reservation, ephemeral/
watcher checks and sequence-conditional store delete. Global runtime shutdown
still joins composition intake/registration before draining residents. No store
append guard, supervisor change, version bump or frontend change is part of this
fix.
