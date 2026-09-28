# 0.43.15

## Restart proves the new build first and rolls back a build that fails to start

- New `hya serve check [--json]`: composes the complete runtime a daemon start would (configuration without the offline fallback, providers, bundles, native tool libraries, plugins, startup recovery) against a private `VACUUM INTO` snapshot of `--db`. It binds no port and takes no lock of the live database, so it runs safely beside a running daemon.
- `hya serve restart` now runs the successor build's `serve check --db <db>` and every new `--verify <cmd>` (`sh -c`, current directory) before it touches the running daemon; a failure prints the output tail and leaves the daemon serving. `restart --json` reports `check: {ok, exe, version, verified}`. New `--exe <path>` restarts into a build other than the invoking one, through the same gate.
- Rollback: every daemon pins the build it runs (executable plus loaded native tool libraries, and the installed `bundles/*.hyabundle`) under `<db>.server.gen/<pid>/`. A successor that records `failed`, exits, or is not ready within 90 s is killed and replaced by the pinned build over the same listener and lock; the old generation parks only if that also fails. `hya serve status` reports `lastRestart: {rolledBack, error}`.
- A build now opens a database migrated by a newer build (unknown applied migrations are tolerated; known checksums must still match), so a rollback can serve it. Migrations must stay additive.

```sh
cargo build -p hya-backend --bin hya
./target/debug/hya serve restart --verify 'cargo test -p hya-core'
```
