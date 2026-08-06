# 0.34.14

- `hya-backend serve` now drains on `SIGTERM`, `SIGINT`, and `SIGHUP` instead of
  dying by signal. The HTTP accept loop stops, the spawn supervisor shuts down,
  and the process exits `0`. Supervisors and wrapper scripts that previously
  observed a signal death now observe a normal exit status.
- The agent process E2E harness (Track P) stops backends with `SIGTERM` to the
  child's process group, waits a bounded period for a clean exit, and escalates
  to `SIGKILL` only if that expires — preserving the no-orphan guarantee while
  letting atexit handlers run.
- Added `HYA_E2E_BACKEND_BIN` so an E2E run can point at an alternate backend
  binary instead of overwriting `target/debug/hya-backend`.
- Fixed three flaky tests: `bundle_cli` temp-path collisions, `frontend_cli`
  `ETXTBSY` when exec'ing a freshly written binary, and admission fixtures that
  raced a concurrent `HOME` change while recomputing a runtime fingerprint.
