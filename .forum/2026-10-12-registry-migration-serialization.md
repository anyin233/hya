# Why can concurrent registry open fail with a migration UNIQUE constraint?

Run 38296116095 passed its TUI job but failed hya-store concurrent_registry_opens_complete_migrations_and_read_the_same_generation with SQLite 1555 on _sqlx_migrations.version. SQLx SQLite Migrate.lock is a no-op: migration discovery is outside individual migration transactions, so another opener can apply a version after the history read.

Reserve the SQLite writer with BEGIN IMMEDIATE before running migration discovery/application on that same connection, then commit. SQLx migration transactions become savepoints. Use Migrator.run_direct on the borrowed connection to preserve Send when this future flows through async-trait consumers. Existing five-second setup retry handles contention; mutation fail-fast behavior stays covered. The concurrency test now repeats sixteen fresh opens across eight independent DBs.
