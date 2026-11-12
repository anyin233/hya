# How do kill, teardown and shutdown preserve episode ownership?

A killed team leaves `ResidentSupervisor::teams` immediately. The separate
session-to-team episode index retains ending members until their claim-keyed
completion. New work receives a fresh main context and counters; revival waits
only for the same member's previous completion.

The supervisor's `claim_writes` boundary covers claim commit plus episode
installation and every orphan Takeover decision across team generations. Kill
never acquires it: a registration committed across a kill installs into that
killed team with its ending requested. Initialization publishes registration
before terminal events. Terminal publication follows in-memory episode removal,
and precedes completion resolution, under this boundary so revival cannot
publish a new registration ahead of the old archive.

## Does cancelling an initializer mean the whole team is shutting down?

Only when the team token was cancelled without an explicit member ending.
Parent archive also cancels an initializer, and must retain that member's first
requested ending. Budget kill cancels member turns and the harness lead turn;
it leaves an unrelated client turn on the root alone. Root delete and shutdown
also cancel the root's client turn.

## How does shutdown stay within one drain plus one sidecar deadline?

After composition intake and registration handlers have joined, the supervisor
runs the engine turn drain alongside episode Shutdown endings. Serial waits
would charge the same stuck turn twice. Each episode then bounds sidecar
termination, commits its ending, removes itself and resolves completion. The
supervisor also reconciles orphan claims and cancels and joins `run_bus` before
`drain` returns. The engine-to-supervisor strong-reference cleanup remains M4-12;
joining this listener alone does not prove engine deallocation.

## What changed in the I5 harness?

Readiness to trip another budget turn must use the old team's cancellation token.
A slow killed member no longer archives before its sidecar cleanup. Using that
member's archive visibility can send the extra mail to an already-archived
sibling, revive it, and accidentally found a new team before the intended task.
The test now checks fresh work immediately after cancellation, then awaits the
slow member's own archive before testing its revival.

## May root binding resolution hold the claim-write boundary?

No. Resolve/select the team before acquiring it, including after waiting for a
previous episode. A cold team's binding may invoke plugin hooks. The boundary
only serializes store work and installation/removal/publication; it must not
make unrelated episode endings wait on a binding hook.

Shutdown snapshots existing orphans before requesting live endings. It uses
Takeover only for those actors; a terminal write that fails during the drain
remains an orphan for a later request, preserving the failed-write crash boundary.
