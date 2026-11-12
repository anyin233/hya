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

## Why must registration reserve its selected team?

M4-3 fix instrumentation confirmed selection → claim-write await → idle
retirement → install into the cancelled team, yielding `Cancelled` from mail
revival. `TeamRegistration` reserves under the supervisor map lock and excludes
retirement through initialization; explicit kills still request endings.
`mail_revival_reserves_the_team_before_claim_registration` forces this gap at
mail's real entry point. Root main resolution now shares root turns' durable
missing-agent fallback; missing child definitions remain errors.

## Who owns main progress during a Workflow?

The core runner holds `WorkflowActivity`: it suppresses autonomous main wakes
while stages run and consumes their synthesis mark at completion. A cold root
has no model turn to resume, so it stays non-autonomous until a user root turn
or explicit main activation. Workflow tests must not manufacture root turn
leases. Drain endings carry their actual `FinishCause`; a user cancel and a
failed leader must not become restartable shutdowns.

## Why must the episode drain its own turn before writing a team ending?

M4-4's temporary SQLx diagnostics reproduced `Sqlite(PoolTimedOut)` with the
lead episode's suspended `run_one_turn` holding the memory store's only pooled
connection in `cached_projection`. The cancellation branch awaited `team.kill`
(and its lead activity write) before polling that future again. The connection
could therefore never be released to the kill or the test's replay. This can
also exhaust a larger pool when enough episodes cancel together.

Await the owned turn before starting those writes. Cancellation is bounded by
`DRAIN_DEADLINE`; close a straggler under its claim before dropping the future.
Sidecar loss requests `Failed("activation sidecar lost")` and uses this same
ending, without an in-process epoch bump. Idle loss drops the handle and keeps
the episode. Every sidecar stop uses `SIDECAR_STOP_DEADLINE`, including idle
loss and unhealthy-handle replacement. Failure reports retain stop diagnostics.
`take_episode` removes the slot/index before archive publication and completion;
the old direct `archive_stopped_agent`, claim replacement and `remove_slot`
paths are gone. Startup recovery and orphan `Takeover` remain separate.

## When does sidecar loss become an ending?

Only after the owned turn and sidecar stop finish, and only if loss interrupted
an actually cancelled turn. `run_one_turn` returns NotStarted/Finished/Cancelled;
loss after Stop or during an empty wake drops the handle and retains any report.
A forced ending requested during drain or sidecar stop wins. Main's straggler
message is closed unfenced before dropping its future, just as member messages
are closed under the claim. Default sidecar health derives from its loss token.

## What keeps a terminal report's parent wake from disappearing?

Before taking the episode, hold the team's committed terminal MailSent sequence
numbers in `routing_mail`. Retirement refuses those holds. The bus listener
releases each hold after on_mail routes it; lag recovery releases only holds
covered by its projection snapshot after re-arming that inbox. This keeps the
slot-removed-before-archive and slot-removed-before-done properties without
retiring/cancelling main in the publication-to-routing interval.
