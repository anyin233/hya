# 0.36.33

## Strip release binaries for a faster cold start (workspace)

- Release builds now strip local symbol tables
  (`[profile.release] strip = "symbols"`). The `hya-backend` release binary
  shrinks from ~43MB to ~33MB (-22%) with no runtime behavior change.
- Backend first-execution cold start is dominated by pre-main work —
  page-in, code-signature verification, and dyld fixups — which scales with
  binary size. Measured on Apple M4 (spawn → HTTP listen, first exec of a
  freshly linked binary, median of 3): ~408ms → ~328ms (-20%). Warm
  restarts are unchanged (~6–11ms).
- Per-step startup timing (pre-main exec/dyld, SQLite connect and
  migrations, config load, engine build, MCP handshake) was gathered with
  temporary `HYA_STARTUP_TRACE` instrumentation; `cargo run -p xtask --
  startup-bench` remains the committed spawn→listen regression harness.
