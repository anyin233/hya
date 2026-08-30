# 0.36.4

## Session control

- Native shell requests now honor their optional Agent and model selections, in
  the same way as the Compat shell routes. Model changes are persisted before
  the synthetic shell Turn runs.

## Spawn admission

- Concurrent same-root spawns now reconcile durable logical releases before a
  final overload decision. A completed operation can no longer leave usable
  governor capacity temporarily unavailable, and multi-member batches remain
  reserved until every member is terminal.

## Error reporting

- Background Compat prompt failures now cap durable provider error text at
  2,048 Unicode scalar values before the Session returns to idle.
