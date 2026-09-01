# Drop the legacy TUI surface

We will delete the legacy terminal UI surface instead of keeping `--mini` as an alias or waiting for full behavior parity. The current frontend is the interactive surface of record; the only legacy behavior preserved is **Resume**, because opening an existing Session is an interactive CLI contract rather than a renderer feature. `--mini` is removed as a real option, so old invocations fail as unknown arguments instead of carrying a dead compatibility branch.

## Consequences

- The legacy TUI crate and its backend controller/render path are removed together; leaving the crate on disk would keep it in the workspace.
- Resume is implemented through the current frontend by navigating to the requested
  Session route immediately. If the Session is unavailable in the connected
  runtime, the route returns to Home and reports the failure with a visible toast.
- No other `--mini`-only behavior is ported. Current frontend behavior is the source of truth after this cutover.
