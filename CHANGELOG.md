# 0.43.2

## Supervisor-owned listener handoff

- `hya serve --listen-fd <FD>` now adopts an already-open Unix TCP listener for foreground server startup without rebinding the port.
- The handoff is fail-closed, sets close-on-exec, and is documented as the foundation for a later successor-process restart protocol; it does not migrate live streams or application state.
