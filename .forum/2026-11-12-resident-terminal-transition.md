# Where is resident termination committed?

The resident store finalizer commits cleanup, handoff, optional parent report and
mail, terminal activity, archive and claim release under one writer reservation.
Prepared voluntary reports carry the observed cursor/work boundary and handoff
generation, and recheck unread mail and live children after handoff generation.
Publish returned envelopes and refund admissions through
`publish_resident_finalization` only after commit.

Stop/failure now archive instead of leaving failed roster rows. Repeated stop
recognizes the archive. Root teardown passes the supervising slot's capability;
it must not claim an actor already owned by the supervisor. A killed team accepts
no registration: spawn and revival wait for all slots and budgets to drain, then
restore `main` on a fresh team. Episode context and mail ack and
authorization changes remain separate work.

The Workflow activation wait must recognize archived actors as terminal and
read the linked parent member row. It must only accept a successful member row
for a `Reported` archive. Forced finalization now replaces a previous episode's
member result even before the revived actor's first turn.
The existing `collect_all` regression now has a bounded wait that reproduces
the former roster-only hang.

# What did the independent terminal-transition review establish?

Real-entry-point regressions reproduced killed-team reuse, missing main/budget
reset, unsearchable forced handoffs, lost failure reasons in `wait`, stale
episode results, blocked busy-root deletion, unfenced PRIOR HANDOFF writes,
stale report retries and shutdown's multi-transaction fallback. Registration
shares an async gate with kill decisions through commit and slot installation;
hooks never hold it. The store takes explicit prepared handoff/reason/status
inputs and builds no prose. Refused reports are dropped. A failed terminal
transition is returned or logged with the session id; no fallback mutates state.
An already-empty killed team resets budgets in `team_for`; it must not wait for
a cleanup notification when no slot remains to produce one.

# Why is report.rs's former Busy assertion not restored?

The slot reserves Busy before the engine appends its Busy event. Sampling those
states separately cannot require durable Busy whenever the slot says Busy; the
turn can also finish between samples. The guaranteed direction is slot finish
before durable Idle. The existing report test retains its bounded retry for the
reservation window, and engine-level stop/recovery tests again send mail through
`engine.mail_send` and assert same-session revival. They no longer substitute a
store-only mail rejection for the engine contract.

## Can a client resume an archived member with a direct turn?

No. Team membership is permanent once `AgentRegistered.agent_session` names the
session in its root log. Check that historical event before turn preparation or
execution-state edits, rather than the live claim or roster: archival and revival
must not open a client-admission window. Use mail/steer for member work. Model/agent
switches and revert/unrevert are refused; a fork copies into an independent root
and does not mutate its member source.
