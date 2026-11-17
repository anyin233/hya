# Three-primitive kernel

`hya exec --bare` runs the `hya-core::kernel` loop alone: five built-in
tools, a minimal system prompt, and the SQLite event log. It loads no bundle.
Use it to run a task with only the kernel's Context operations. See
[ADR-0033](adr/0033-three-primitive-kernel.md) for the design and invariants.

```sh
hya exec --bare "say hi" --db /tmp/x.db
```

The working directory is the caller's cwd. `--bare` conflicts with `--pure`,
`--time-budget`, `--json`, and routing to a running server; these
combinations error out. See the [CLI reference](cli.md#--bare).

## Three primitives

Context is what the model sees: the event log folded by the shared
`hya-proto` reducer into the system prompt, tool list, and live work.
Only three primitives change it:

| Primitive | Meaning | Events |
| --- | --- | --- |
| Model append | The model's text, reasoning, and tool calls | `StepStarted`, `TextStart/Delta/End`, `ReasoningStart/Delta/End`, `ToolCallRequested`, `StepFinished`, `UsageRecorded`, inside assistant `MessageStarted`/`MessageFinished` |
| Tool append (tool) | An outside observation paired with a model call | `ToolResult`, `ToolError` |
| Tool append (user) | User input through the same observation channel | user `MessageStarted`, `TextStart/Delta/End`, `MessageFinished` |
| Rewrite | Remove earlier Context while retaining its log | a system marker message and `ContextCompacted` with strategy `rewrite` |

The log only grows. Model text remains a claim; only tool append introduces
outside observations. Unknown tools, invalid input, execution errors, and
cancellation return as failed tool results. Tools run sequentially, with no
retry, model fallback, or step limit.

## Built-in tools

The following JSON shapes use `string` and `int` as type placeholders. All
input fields are required. Relative paths resolve against the run's working
directory.

| Tool | Input JSON | Success result JSON |
| --- | --- | --- |
| `bash` | `{"command": string}` | `{"exit_code": int, "output": string}` |
| `read` | `{"path": string}` | `{"content": string}` |
| `write` | `{"path": string, "content": string}` | `{"bytes": int}` |
| `edit` | `{"path": string, "old": string, "new": string}` | `{"replaced": 1}` |
| `rewrite` | `{"summary": string}` | `{"removed_messages": <n>, "note": "The summary in this call is your own text, not an observation."}` |

`bash` runs `sh -c` in the working directory and merges stdout and stderr.
A nonzero exit is still a success result for the model to interpret. Failure
to start returns `ToolError`; cancellation kills the process.

`read` returns UTF-8 text; missing files and invalid UTF-8 return `ToolError`.
`write` creates parent directories and returns the byte count; I/O errors
return `ToolError`. `edit` requires exactly one occurrence of `old`; zero or
multiple matches return `ToolError` stating the count.

`rewrite` keeps its `summary` in the call input in the current step. It does
not store a second copy or turn the summary into an observation. `<n>` is the
number of removed messages. With no earlier content it returns
`ToolError("nothing to remove")` and emits no `ContextCompacted`.

## Rewrite rules

A rewrite removes everything before the current step: earlier messages and
earlier steps in the current assistant message. It keeps the current turn's
user message (`StepCut.kept_head`) and the current step, including its
reasoning, calls, and results. It removes whole steps, leaving no dangling
call/result pairs and never removing reasoning on its own.

The harness writes a system marker message and `ContextCompacted` in one
batch. The marker text is:

```text
<COMPACT_CONTEXT_MARKER>[hya] Earlier context was removed by the <model|harness>. It stays in the session log and cannot be read back.
User messages from the removed part, verbatim:

<each removed user message's text, in order, separated by "--- user message N ---">
```

`<model|harness>` names the initiator. User text is repeated verbatim; the
model cannot replace it with a summary. Removed originals stay in SQLite,
but the kernel provides no way to read them back into Context.

`ContextCompacted` records `strategy: Rewrite` (wire name `rewrite`),
`from_message` (first visible message), `to_message` (current assistant
message), `folded_count`, `input_tokens_est`, `threshold`, and
`step_cut: Some(StepCut { through_part, kept_head })`. `through_part` is the
last part before the current step; `kept_head` is the current turn's user
message. If the assistant message has no earlier step, `step_cut` is `None`
and `to_message` is the preceding message.

Before a round, the harness rewrites when the last assistant message's
`usage.last_round` satisfies:

```text
last.prompt() + last.output > max_context * 8 / 10
```

This is a strict 80% threshold using the provider's model capabilities. If
capabilities or last-round usage are missing, no rewrite is triggered; an
oversized-request provider error surfaces unchanged. Harness rewrites use
the same removal range as model rewrites.

Known limitation: image attachments in removed user messages are not kept
with the repeated text.

## System prompt

`kernel::prompt::system_prompt(cwd)` returns the following text, with `{cwd}`
replaced by the working directory:

```text
You are a coding assistant running inside hya, a coding agent harness. Help users inspect files, run commands, and modify code.

<tools>
- read: read a file
- bash: run a shell command
- edit: replace text in a file
- write: create or overwrite a file
- rewrite: remove everything before this call from the conversation, keeping the summary you pass
</tools>

<rules>
- Use bash for file operations like ls, rg, find
- Only tool results tell you what is true outside this conversation. Your own text is a plan or a claim until a tool result supports it.
- Before reporting that something works, run the check and read its result.
- A rewrite summary is your own text, not an observation. Put in it what a later correction would need: the task, what was checked and how, what failed and why, what is still open.
- Be concise in your responses
- Show file paths clearly when working with files
</rules>

<cwd>
{cwd}
</cwd>
```

## Differences from full mode

Bare mode loads no bundle. It has no permission checks, hooks, subagents,
Workflows, compaction ladder, output truncation, timeouts, or path bounds.
These capabilities belong to full mode and its bundles. The kernel retains
its five tools, minimal prompt, 80% harness rewrite, and event logging.
`--pure` only removes external project/user context from full mode; it does
not select the kernel alone.

## How full mode runs on the kernel

Full-mode turns run the same kernel loop as `--bare`; there is no second
round loop. Each round reads the projection, lets the harness rewrite it,
builds the request with `kernel::request` from the (re-read) projection,
streams one model round, and runs its tool calls in order. The full mode
plugs into that loop at fixed points:

| Point | Bare mode | Full mode |
| --- | --- | --- |
| Before the round (`Rewriter`) | 80% harness rewrite | Re-validate the actor claim, rebind a Root turn to a new runtime generation, resolve the round's model, then walk the [compaction ladder](compaction.md) |
| Request | Kernel prompt and five tools | The agent's system prompt, filtered tool schemas plus `rewrite`, and reasoning effort, then the `chat.params` hook |
| Opening the stream | The given model | Cross-model fallback or the Workflow route, under a governor stream permit |
| Failed round | Ends the run | Retried up to twice when it failed before any text or tool call on a link error |
| Tool call (`ToolGate`) | Execute the tool | Hooks, permissions, the full `ToolCtx`, file snapshots, output capping, steer notices |
| Unrun calls after a cancel | Failed as `cancelled` | Left open; the turn end closes them as `CANCELLED` |
| End | The round's finish | Also ends after an accepted `report` |

The model's `rewrite` call is offered and handled by the kernel in both
modes. Everything that shapes a request is in the log: the ladder's
evictions are `ContextEvicted` events whose `parts` carry each evicted part
with its exact notice, so the projection renders the notices; `TurnBindingRecorded` records the
SHA-256 digests of the system prompt and tool list at turn start and again
whenever a round's differ. Only the digests are logged, not the text, so a
restart replays the same request while the bundles are unchanged.
