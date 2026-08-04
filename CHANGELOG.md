# 0.34.12

- Add durable multi-resource admission with a 100-active / 156-non-active /
  256-envelope capacity vector, atomic batch claim, Queued promotion, parent
  suspension fairness, and restart-stable reconstruction.
- Route foreground transient batches and single-member background work through a
  process-local admission owner that rehydrates on wake, preserves ordinal reply
  identity, delays background running replies until Started registration, and
  cancel-first terminalizes with `SpawnError::Cancelled`.
- Own spawn-supervisor lifecycle via public `BuiltSessionEngine`: explicit
  shutdown drains handlers; Drop is nonblocking stop/abort; backend commands and
  serve paths await shutdown.
- Certify R10 capacity gates (100/156/256, item-257 zero allocation, bounded
  promotion after Started release) and lock Consult30 live queued reply barriers.
