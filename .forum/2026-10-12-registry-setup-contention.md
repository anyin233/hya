# Why can a successful bundle installation have no API endpoint?

The install RPC can persist the package while `refresh.errors` reports a failed
activation. In CI run 38268751961, `/info` was absent after installation. A local
120-install probe reproduced SQLite `database is locked` during registry setup.
The shared test now checks refresh errors and the activated bundle catalog before
calling its endpoint, so future failures report the actual activation cause.

The registry intentionally uses a zero busy timeout so writes fail fast. Keep
that contract. Retry only BUSY/LOCKED while opening connections or migrating,
with bounded asynchronous backoff. Concurrent opens of a fresh registry provide
focused regression coverage; the existing writer-contention test remains green.
