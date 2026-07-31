# 0.34.6

- Reconcile startup, deferred, and Compat-controlled MCP sources through one
  desired/observed coordinator and publish only complete validated runtime
  snapshots.
- Preserve the prior effective generation on failed handshakes or collisions,
  reject stale async results, and make removals visible atomically to the next
  turn while retained turns keep their old source bindings.
- Give MCP and plugin contributions stable source identities, reject canonical
  and alias collisions before publication, and retain source clients in the
  immutable runtime snapshot.
- Validate configured plugin identity and the complete deterministic initialize
  declaration on respawn; declaration drift closes the new process and fails
  subsequent calls closed without claiming plugin hot reload.
