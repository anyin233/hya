# 0.36.17

## Heap snapshots

- Write real V8-compatible heap snapshots in the hya cache with owner-only permissions.
- Report the actual file path or failure instead of an undefined success; heap dumps may contain sensitive in-memory data.
