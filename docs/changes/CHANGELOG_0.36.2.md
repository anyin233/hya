# 0.36.2

## Session lifecycle

- Native prompt, command, shell, Event replay, and SSE routes now return 404
  for unknown or deleted Session IDs. A deleted Session can no longer be
  recreated by later native admission.
