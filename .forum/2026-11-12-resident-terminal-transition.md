# Where is resident termination committed?

The resident store finalizer commits cleanup, handoff, optional parent report and
mail, terminal activity, archive and claim release under one writer reservation.
Prepared voluntary reports carry the observed cursor/work boundary and handoff
generation, and recheck unread mail and live children after handoff generation.
Publish returned envelopes and refund admissions through
`publish_resident_finalization` only after commit.

Stop/failure now archive instead of leaving failed roster rows. Repeated stop
recognizes the archive. Root teardown passes the supervising slot's capability;
it must not claim an actor already owned by the supervisor. A killed empty team
gets a new cancellation token when reused. Episode context and mail ack and
authorization changes remain separate work.

The Workflow activation wait must recognize archived actors as terminal and
read the linked parent member row. It must only accept a successful member row
for a `Reported` archive, since a forced stop can retain an older report row.
The existing `collect_all` regression now has a bounded wait that reproduces
the former roster-only hang.
