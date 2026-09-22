# 0.37.12

## Native tool context bridge

- Bind Rust bundle tool calls to their active `ToolCtx` through call-scoped host capabilities.
- Expose call identity and workdir metadata plus host-enforced resource permission checks to native processes.
