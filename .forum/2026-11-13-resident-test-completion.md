# Does an archived resident prove a test's work completed?

No. Initialization, provider and terminal failures archive residents too. The
shared `run_residents` fixture requires each child's last assistant message to
finish with Stop or Length before accepting the result. Its failure regression
runs a provider that ends with Error and requires the fixture to reject it.
Settlement waits subscribe before registration and use lifecycle bus events,
so no fixed-delay polling is needed. This preserves the completion oracle in
migrated concurrency and sidecar tests: six failed children cannot masquerade as
six completed requests with zero concurrent streams.

## Does a second provider poll prove a stopped member restarted?

No. A provider shared with the root counts its permitted quiescence synthesis
as well. Ending the last member can wake the root before `end_member` returns.
Temporary session-ID tracing reproduced the running-stop test's global count
of two; a channel barrier that observed child first and root next made its old
`polls == 1` assertion fail deterministically. No extra child turn occurred.

`ResidentStopBarrierProvider` now sends the request's `SessionId`. The stop test
waits for root synthesis, then rejects any extra provider entry after the
member's ending has completed. This checks the member's turn count without
racing the root. Keep that distinction when reusing a provider across sessions.
