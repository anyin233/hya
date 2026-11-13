# Does a parent’s own task-directive DM copy count as unread work?

No. DM history fans out to both peers, and raw inbox cursors count both peers’
posts. Readers use `TeamProjection::unread_incoming_mail(handle, through)` to
exclude outgoing copies without changing cursor positions. Outgoing directives
must not prolong headless execution, inflate channel unread counts, or retain a
synthesis after incoming results were explicitly read.

# When does a newly registered directive become pending?

At episode installation, before the initializer publishes registration envelopes.
Parked actors have no directive and remain idle. A `session_start` hook can hold
this gap deterministically for wait/cancel/quiescence tests. The bus notification
then coalesces with the pending wake; `begin_work` still admits one prompt.
Quiescence waits for pending members; the lead’s own pending mail still coalesces
with its synthesis. Slot idleness is in memory, so the final bus event is not a
completion notification for it.
