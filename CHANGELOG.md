# 0.34.7

- Recover durable resident actors before spawn/mail readiness by atomically
  advancing a TTL-free actor epoch, aborting old running work, and rescheduling
  only committed queued messages.
- Fence resident event, tool-result, mailbox, child, and spawn transitions with
  the current actor claim so late old-incarnation commits fail typed-closed and
  never advance replay or live projection state.
- Bind resident-originated durable admissions to actor identity and epoch, then
  converge takeover, cancellation, completion, and refund on idempotent
  terminal transitions without crediting a fresh governor after restart.
- Persist one minimal resident-work-started marker and durable inbox cursor so
  repeated recovery is projection-stable and does not retry in-flight effects.
