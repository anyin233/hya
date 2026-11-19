# 0.48.0-dev.9

- **Bare mode sends the configured reasoning effort.** `hya exec --bare` now puts the model's configured reasoning default on every request. Earlier bare builds sent no `reasoning` field, so the model ran at the provider's default effort.
- **`bash` timeout in the kernel.** The bare kernel's `bash` tool takes an optional `timeout` in seconds (default 300, at most 86400). At the limit it kills the command's process group and returns `exit_code: null`, the captured `output` and a `timed_out` note instead of blocking the run; a `--time-budget` run still caps the limit and no command is started once the budget is spent. After a command ends, `bash` waits at most one second for output from a process that left the group and keeps the pipe open, so such a process can no longer hang the call.
- **Changed result.** A budget timeout of the bare `bash` tool is now a tool result with `timed_out`, not the tool error `bash timed out under the run's time budget`.
