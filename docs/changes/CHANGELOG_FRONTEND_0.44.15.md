# 0.44.15

## TUI

- Recognize `task` calls using `tasks[]` as batch cards with the member label and count, retaining `members[]` compatibility and ignoring unused single-task labels in batch mode.

- Precompile JSX in release packages and load gRPC only for gRPC connections.
- Overlap frontend initialization with initial backend reads and extension-host startup.
- Add browser cold/warm startup diagnostics and optional latency budgets.
