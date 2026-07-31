# 0.34.3

- Reject overloaded background transient and resident spawns before creating
  child sessions or lifecycle events.
- Bound the spawn request queue from the existing per-run admission budget and
  return a typed overload immediately when admission or queue capacity is
  exhausted.
- Align the startup benchmark helper with the workspace formatting and lint
  gates.
