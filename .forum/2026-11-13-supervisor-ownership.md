# Who keeps a supervisor alive, and what does shutdown join?

The built runtime owns it. Both engine seams are weak; the listener keeps a Weak
between deliveries and performs only synchronous indexed routing. Its explicit
cancel/JoinHandle remains separate from the task tracker that owns all episode,
main and per-team FIFO workers. Drain first requests and awaits episode endings,
then joins listener and task futures. A done watch can resolve before the task
drops its captured engine, so waiting only on watches cannot prove I19.

# Why protect retirement while envelopes are queued?

Terminal mail already holds routing_mail until bus delivery. FIFO routing also
counts queued/in-progress events under the team lock; otherwise the final episode
could retire main after enqueue but before its report was delivered. Killed
generations remain reachable through the existing episode index until their
tasks remove them. The current-root map and per-root driver state retain their
existing roles; replacing the latter with a team-local count would regress R4.
