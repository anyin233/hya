# Does an archived resident prove a test's work completed?

No. Initialization, provider and terminal failures archive residents too. The
shared `run_residents` fixture requires each child's last assistant message to
finish with Stop or Length before accepting the result. Its failure regression
runs a provider that ends with Error and requires the fixture to reject it.
Settlement waits subscribe before registration and use lifecycle bus events,
so no fixed-delay polling is needed. This preserves the completion oracle in
migrated concurrency and sidecar tests: six failed children cannot masquerade as
six completed requests with zero concurrent streams.
