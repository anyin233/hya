# 0.36.23

## Abort in-flight provider HTTP

- Dropping a provider EventStream (Escape / session abort) now closes the HTTP body immediately instead of waiting for the SSE idle deadline.
- Keepalive-only streams no longer keep the model connection open after the turn is cancelled.
