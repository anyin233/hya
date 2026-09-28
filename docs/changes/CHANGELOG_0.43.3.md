# 0.43.3

## Successor daemon restart handoff

- `hya serve restart` now transfers the listening socket and database lock to a
  healthy successor generation without rebinding the port.
- Active root turns receive a durable handoff boundary and resume exactly once
  after successor bootstrap; client streams reconnect and reload durable state.
- Added quiescence, pending-interaction lifecycle handling, and fallback
  recovery documentation for restart failures.
