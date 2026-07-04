# Drop the legacy TUI surface

We will delete the legacy terminal UI surface instead of keeping `--mini` as an alias or waiting for full behavior parity. The current frontend is the interactive surface of record; the only legacy behavior preserved is **Resume**, because opening an existing Session is an interactive CLI contract rather than a renderer feature. `--mini` is removed as a real option, so old invocations fail as unknown arguments instead of carrying a dead compatibility branch.

## Consequences

- The legacy TUI crate and its backend controller/render path are removed together; leaving the crate on disk would keep it in the workspace.
- Resume is implemented through the current frontend by validating the requested Session against the connected runtime before navigating. If the Session is unavailable in that runtime, the frontend stays on its current route and reports the failure visibly.
- No other `--mini`-only behavior is ported. Current frontend behavior is the source of truth after this cutover.
