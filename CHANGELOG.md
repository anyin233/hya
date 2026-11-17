# 0.48.0-dev.2

## Full-mode turns run on the kernel

- Full-mode turns now run on the kernel's round loop (ADR-0033); the engine no longer has a loop of its own. Every request is built by `kernel::request` from the session projection. Full-mode behavior plugs into fixed points of that loop: the compaction ladder, rebinding, and model resolution run before each round; the agent's prompt, tools, and `chat.params` shape the request; model fallback, Workflow routes, and round retry handle opening and failed rounds; hooks, permissions, and steer notices wrap each tool call. Turn behavior is otherwise unchanged. See [How full mode runs on the kernel](docs/kernel.md#how-full-mode-runs-on-the-kernel).
- The model's `rewrite` tool is now also offered in full mode. It removes everything before the current step, keeps the summary the model passes, and repeats the removed user messages verbatim, as in `hya exec --bare`.

## Requests replay from the log

- `ContextEvicted` has a new field, `parts`, that lists each evicted tool part with the exact notice requests send instead of its output. The projection keeps the notice beside the part, so later requests, and requests after a restart, send the same bytes. Evictions no longer live only in memory, and `ContextEvicted` is now recorded whenever eviction saved tokens. Logs written before this field read it as empty.
- `TurnBindingRecorded` has two new fields, `system_sha256` and `tools_sha256`, holding the SHA-256 digests of the system prompt and tool list a turn sends. It is recorded again mid-turn when a round's digests change, as after a rebind. Only digests are logged, not the text.
