# ADR-0035: Full mode follows the kernel

Date: 2026-11-18
Status: Accepted

## Context

ADR-0033 made full mode run on the `hya-core` kernel loop, and ADR-0034 moved
the todo list and the workspace record into the kernel. Several full-mode
features still did the kernel's job a second way:

- **Context reduction.** Full mode replaced the kernel's rewriter with the
  `hya/compaction` ladder, which had its own trigger and five mechanisms,
  two of them a side model writing a summary. The kernel's `rewrite` tool and
  goal checkpoint ran next to it, and the checkpoint described a removal the
  ladder did not perform.
- **Tools.** `hya/base-tools` and the kernel both implement `read`, `write`,
  `edit` and `bash`, with different schemas under the same names.
- **System prompt.** Full mode sent the agent's prompt without the kernel
  prompt, so the rules for the kernel's `rewrite` and `todo` tools reached
  the model only as tool descriptions.
- **Workspace state.** The `workspace.state` host capability hashed the Git
  work tree, although ADR-0034 C6 says the workspace is never hashed.

## Decision

Where full mode and the kernel do the same thing, the kernel's way is the
one that runs. A bundle extends the kernel or replaces one of its tools under
the same name.

- **F1** The kernel drives context reduction in both modes. Before a round
  it computes the pressure (the larger of the last recorded round's usage
  and an estimate of the request the round would send, against 80% of the
  window), lets the registered rewriters reduce first, and performs its own
  Rewrite when the request is still over the threshold. The trigger now
  counts tool results that arrived after the last round, in bare mode too.
- **F2** `hya/compaction` is a list of extension steps, not a replacement:
  `shake`, then `remote`, in that fixed order. The model-written folds
  (`soft`, `handoff`) and the model-free fold (`snapcompact`) are removed.
  A fold of the transcript prefix is the kernel's Rewrite: user messages
  and the todo list verbatim, and the summary the model passes to `rewrite`.
- **F3** `/summarize` is a kernel Rewrite (after `remote` on a route that
  has it). It makes no side model call.
- **F4** The terminal handoff of an archived subagent (ADR-0015) keeps the
  engine's summarizer. The kernel has no equivalent, so it is not a
  duplicate.
- **F5** In full mode `hya/base-tools`' `read`, `write`, `edit` and `bash`
  replace the kernel's four under the same names. The kernel's four run only
  in bare mode. The names `rewrite` and `todo` are reserved for the kernel:
  no bundle tool or alias may take them.
- **F6** `ls`, `glob`, `find` and `grep` stay in `hya/base-tools` as the
  faster way to do what the kernel sends to `bash`. They replace the kernel
  prompt's file-operations rule, not a kernel tool.
- **F7** `apply_patch` is offered to GPT-family models only, next to `edit`
  and `write`.
- **F8** A full-mode system prompt is the kernel prompt, composed from the
  tools the round offers, followed by the agent's prompt, the recorded
  initial context and the team reference. The kernel lists its own tool
  names with its own text whoever implements them, so replacing a tool under
  the same name changes no prompt text. Other first-party tools are listed
  with the line their tool family declares in `exposure.yaml`.
- **F9** `workspace.state` returns a session-scoped revision of the kernel's
  workspace record (`"<session id>:<file revision>"`), or `null` where the
  record cannot see new files. Nothing hashes the work tree.

## Consequences

- After a reduction the model sees the user messages, the todo list and its
  own `rewrite` summary; there is no harness-written summary. The goal
  checkpoint at 60% is what asks the model to record results it still needs.
- The `compaction.*` threshold keys and `method_order` are gone, with their
  environment variables. The parity with oh-my-pi's `compaction.methodOrder`
  no longer holds. `compaction.summary_max_tokens` and
  `HYA_COMPACTION_SUMMARY_MAX_TOKENS` remain as the output cap for the
  [terminal handoff](../compaction.md#terminal-handoff) in F4.
- Every full-mode request's system prompt changes, so request digests and
  traces are not directly comparable with runs before this change.
- `hya-main` no longer tells the model to keep working while a todo item is
  open (ADR-0034 C4).
- A value from `workspace.state` matches only within one session: a cached
  scout answer is not reused by a later session.
- This replaces ADR-0033 D5's reading that a moved-out feature may take over
  a kernel primitive, and the SHA-256 identity described in ADR-0032.

See [kernel.md](../kernel.md), [compaction.md](../compaction.md),
[base-tools.md](../base-tools.md) and
[plugin-protocol.md](../plugin-protocol.md).
