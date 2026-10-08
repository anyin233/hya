# 0.44.15

- Precompile JSX in release packages and load gRPC only for gRPC connections.
- Overlap frontend initialization with initial backend reads and extension-host startup.
- Add browser cold/warm startup diagnostics and optional latency budgets.
