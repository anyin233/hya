# 0.36.6

## Session cache ordering

- Normalize bootstrap Session rows with the same code-unit comparator used by
  binary search, preventing mixed-case IDs from becoming duplicate or
  unreachable.
