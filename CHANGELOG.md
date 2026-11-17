# 0.48.0-dev.1

## Three-primitive kernel

- New module `hya_core::kernel` (ADR-0033): a minimal agent loop where the Context changes only through three primitives — model append, tool append, and rewrite — each recorded as events in the SQLite log. It brings five built-in tools (`bash`, `read`, `write`, `edit`, `rewrite`) and a short system prompt. Failures, unknown tools, and cancellation come back as failed tool results; there are no retries, fallbacks, or step caps. See [Kernel](docs/kernel.md).
- `hya exec --bare "<prompt>"` runs the kernel alone: no bundles, hooks, or permission checks, the cwd as working directory, the configured default model (or `--model`). It prints the final text and leaves the full event log in `--db`. `--bare` conflicts with `--pure`, `--time-budget`, and `--json`, and exits 75 instead of routing to a server that holds the database. See [`hya exec --bare`](docs/cli.md#--bare).

## Context rewrite

- `CompactionStrategy::Rewrite` (wire name `rewrite`) marks a `ContextCompacted` written by the kernel's rewrite: it removes everything before the current step, repeats removed user messages verbatim in a marker message, and keeps the originals in the log. The model starts one with the `rewrite` tool; the harness starts one when the last round used more than 80% of the model's context window.
