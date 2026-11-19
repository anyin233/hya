# 0.48.0-dev.16

- **Store lock wait.** A write to the session database waits up to five minutes for a lock held by another connection, instead of 5 seconds, the same as the wait for a connection. A run on a heavily loaded disk ended with `sqlite: (code: 5) database is locked` when one write outlasted the old limit.
