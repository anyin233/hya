# Why did the resident guidance test see an edited AGENTS.md in CI?

The fake provider captured requests from the child and the team root. Once the child went idle, quiescence could start the root’s first model turn after the test edited the shared workdir’s `AGENTS.md`. The root correctly read the edited file when it recorded its own initial context. The child had already recorded its context and kept it on the next activation.

Wait for and inspect provider requests by session ID when testing a resident. A root synthesis request can satisfy a shared request count before a mail-driven child activation starts. CI run 45517334593 and the local CPU-contention reproduction both failed on a root `build` prompt.
