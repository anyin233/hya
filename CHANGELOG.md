# 0.48.0-dev.8

Full mode now follows the kernel wherever both did the same job ([ADR-0035](docs/adr/0035-full-mode-follows-the-kernel.md)).

- **Context reduction.** The kernel drives it in bare and full mode. Before a round it measures the request (the larger of the last round's usage and an estimate that includes newer tool results) against 80% of the window, lets `hya/compaction` reduce first, and performs its own Rewrite when the request is still over. `hya/compaction` keeps `shake` and `remote`, in that order; `soft`, `handoff` and `snapcompact` are removed. `/summarize` is a kernel Rewrite and makes no model call.
- **Breaking: configuration.** `compaction.token_threshold`, `context_fraction`, `reserve_tokens` and `method_order`, with their `HYA_COMPACTION_*` variables, are removed. The `compaction.before` hook's replacement instructions are no longer used.
- **System prompt.** A full-mode system prompt starts with the kernel prompt, composed from the tools the round offers, followed by the agent's prompt, the initial context and the team reference. Tool families declare one-line tool descriptions (`prompt`) and the file-operations rule in `exposure.yaml`. The Environment block no longer repeats the working directory. `hya-main` no longer asks the model to keep working while a todo item is open.
- **Tools.** `apply_patch` is offered to GPT-family models, next to `edit` and `write`. The names `rewrite` and `todo` are reserved for the kernel: a bundle tool or alias with either name is rejected.
- **Breaking: `workspace.state`.** The host capability returns a session-scoped file revision (`"<session id>:<revision>"`, or `null` without a native file watch) instead of a SHA-256 of the Git work tree. A value matches only within one session.
