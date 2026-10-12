# 0.45.6

- Retry transient SQLite locks while opening or migrating the bundle registry, so concurrent catalog refreshes do not leave an installed bundle inactive. Bundle writes still fail immediately on writer contention.
