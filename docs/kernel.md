# Three-primitive kernel

`hya exec --bare` runs the `hya-core::kernel` loop alone with six built-in
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

The kernel folds the event log with the shared `hya-proto` reducer.
The model sees the prompt, tool schemas, and live transcript (`m`).
The session also tracks completion criteria (`g`) as an ordered todo
list. The model sees that list in tool results and Rewrite markers.
Three primitives update Context.

| Primitive | Meaning | Events |
| --- | --- | --- |
| Model append | The model's text, reasoning, and tool calls | `StepStarted`, `TextStart/Delta/End`, `ReasoningStart/Delta/End`, `ToolCallRequested`, `StepFinished`, `UsageRecorded`, inside assistant `MessageStarted`/`MessageFinished` |
| Tool append (tool) | An observation paired with a model call, including changes to the session todo list and observed workspace files | `ToolResult`, `ToolError`, `TodosUpdated`, `FilesChanged`, `FilesObserved` |
| Tool append (user) | User input through the same observation channel | user `MessageStarted`, `TextStart/Delta/End`, `MessageFinished` |
| Rewrite | Remove earlier Context while retaining its log | a system marker message and `ContextCompacted` with strategy `rewrite` |

The log only grows. Model text remains a claim; only tool append introduces
outside observations. Unknown tools, invalid input, execution errors, and
cancellation return as failed tool results. Tools run sequentially, with no
retry, model fallback, or step limit.

## Request views

Each round reads a `kernel::View` after the harness has had a chance to
rewrite the context. The view borrows the folded session projection.
`View::cut()` gives the last durable event sequence in that projection,
inclusive. `Harness::request(&View)` takes this view and supplies the
resolved harness inputs needed to build the request.

Each `StepStarted` records a `view` with the numeric `cut` and a
`request_sha256` string. The digest covers the harness's completed system
prompt, tool schemas, and messages. Retries reuse the same view reference.
See the event reference for the digest encoding and optional field.
[Step markers](architecture/event-model.md#step-markers-provider-rounds)

To verify a bare-mode log offline, run the ignored replay test from the
repository root with the source revision used to record the log:

```sh
HYA_BARE_REPLAY_DB=/path/to/hya.db \
  cargo test -p hya-core --test replay_bare -- --ignored --nocapture
```

The test opens the database read-only. It rebuilds each recorded bare-mode
request from the session's model, working directory, log prefix, and the
kernel's fixed prompt and tools. It prints the equal and unequal counts
and fails on any unequal view. Full-mode replay also needs the resolved
round configuration, any referenced attachment blobs, and reproducible
hook output. Legacy URI media entries can acquire fresh part IDs during
request assembly.

## Built-in tools

The following JSON shapes use `string` and `int` as type placeholders. All
input fields are required. Relative paths resolve against the run's working
directory.

| Tool | Input JSON | Success result |
| --- | --- | --- |
| `bash` | `{"command": string}` | `{"exit_code": int, "output": string}` |
| `read` | `{"path": string}` | `{"content": string}` |
| `write` | `{"path": string, "content": string}` | `{"bytes": int}` |
| `edit` | `{"path": string, "old": string, "new": string}` | `{"replaced": 1}` |
| `rewrite` | `{"summary": string}` | `{"removed_messages": <n>, "note": "The summary in this call is your own text, not an observation."}` |
| `todo` | `{"ops": [operation, ...]}` | Full list as a JSON string. See [Session todos](#session-todos). |

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

## Session todos

Use `todo` in either mode to record the criteria that will tell you the task
is done and update them as work progresses. The model chooses when to use
it. The kernel executes it directly, like `rewrite`.

| Operation | Required JSON fields | Effect |
| --- | --- | --- |
| Add | `{"op":"add","content":string}` | Append a `pending` item. The kernel assigns the smallest positive decimal string id it has not issued. |
| Edit | `{"op":"edit","id":string,"content":string}` | Replace an item's text. |
| Status | `{"op":"status","id":string,"status":string}` | Set the status to `pending`, `in_progress`, `blocked`, or `completed`. |
| Remove | `{"op":"remove","id":string}` | Remove the item. |

Operations apply in order. For example, in a new session the following call
adds a criterion and starts it in one batch.

```json
{"ops":[{"op":"add","content":"Run the focused tests"},{"op":"status","id":"1","status":"in_progress"}]}
```

The result string is `Current todos:\n- #1 [in_progress] Run the focused tests`.
Each additional item appears on its own row in list order. An empty list is
`Current todos: (empty)`. Pass `{"ops":[]}` to read the current list. Blank
content, an unknown item id, or a malformed operation fails the whole batch
with `ToolError`. A changed list is appended as a `TodosUpdated` full snapshot
in the same batch as its `ToolResult`. Empty and unchanged lists can be read
without a new update. The existing session todo API reads the replayed list.

## Workspace file observations

Bare and full mode record when a file seen by the model may have changed.
For example, the model reads `a.txt`, and while its next request is being
answered another process rewrites `a.txt`. Before the following round builds
its request, the kernel `stat`s every known file and appends one
`FilesObserved` event with `external: true` naming `a.txt`. Nothing about the
change enters the request: the model sees the new contents only if it reads
the file again. The stamps are in the log, so in full mode the same check
also catches edits made between turns, including across a backend restart.

After a tool call, the kernel stamps the files it read or changed and
records `FilesObserved { session, external: false, files }` alongside
the tool outcome in one transaction. The log also records the prior state
of files changed by the tool as `FilesChanged`, which full-mode revert
uses to restore files. Before constructing each round's View, after any
rewrite, the kernel checks each known path with one `stat`. It records a
single `FilesObserved` with `external: true` when at least one stamp differs,
then takes the View from the updated projection. The `session.files`
projection retains the latest stamp per path across turns and restarts.
The request renders the transcript and tool results. File stamps stay
in the log and projection.

A stamp is `{ path: string, mtime_ns: number | null, size: number | null }`.
The path is absolute. Modification time is a signed integer in nanoseconds
since the Unix epoch and size is an integer in bytes. A deleted file or a
path replaced by a non-file has `null` for both values. A metadata read
that fails for another reason leaves the previous stamp in place.

Bare mode observes `read`, `write`, and `edit` targets and files changed
according to `bash`'s Git snapshot. Full mode also observes `apply_patch`
targets, LSP's explicit file, and Grep's explicit file or structured result
paths. The same coverage applies when those tools run through a nested
call. In either mode, the per-round check has no content reads, directory
scan, Git call, or configuration.

- New or changed files that no tool exposed are undiscovered. `ls`, `glob`,
  and `find` list paths without reading file contents. Catalog-backed
  `skill` results, resource handles such as `local://` and `artifact://`,
  and internal accesses by bundles, plugin tools, or MCP servers are outside
  file observation. Grep only exposes its explicit file and its displayed
  result paths.
- A rewrite with the same modification time and byte size is undetectable.
- Shell snapshots cover content changes visible to Git from the command's
  working directory. Files outside that work tree, ignored files, and
  internal output files are excluded. Metadata-only writes can also escape
  capture. A shell snapshot records an outside edit made while the command
  runs as a change of that command (`FilesChanged`), not as an external
  observation, because Git measures the combined delta.

See the [event reference](architecture/event-model.md#file-snapshots-and-revert)
and the [revert documentation](architecture/runtime.md#file-snapshots-and-revert).

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
model cannot replace it with a summary. When the session has todos, the
marker appends a blank line followed by the current list exactly as rendered
by the tool. Full-mode compaction markers carry the same list. The `shake`
rung preserves the most recent visible `todo` result when it evicts stale
tool outputs. Removed originals stay in SQLite, but the kernel provides no
way to read them back into Context.

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
- todo: record and update the task's completion criteria; the current list survives rewrites
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
its six tools, minimal prompt, 80% harness rewrite, and event logging.
`--pure` only removes external project/user context from full mode; it does
not select the kernel alone.

## How full mode runs on the kernel

Full mode is the `hya-engine` crate (`SessionEngine` and everything behind
the server and CLI); the kernel and `CoreError` are all of `hya-core`.
Full-mode turns run the same kernel loop as `--bare`; there is no second
round loop. Each round reads the projection, lets the harness rewrite it,
builds the request with `kernel::request` from the resulting view,
streams one model round, and runs its tool calls in order. The full mode
plugs into that loop at fixed points:

| Point | Bare mode | Full mode |
| --- | --- | --- |
| Before the round (`Harness::before_round`) | 80% harness rewrite (`HarnessRewriter`) | Re-validate the actor claim, rebind a Root turn to a new runtime generation, resolve the round's model, then run the engine's rewriter: the [compaction ladder](compaction.md) of the `hya/compaction` internal bundle |
| Request | Kernel prompt and six tools | The agent's system prompt, filtered tool schemas plus `rewrite` and `todo`, and reasoning effort, then the `chat.params` hook |
| Opening the stream | The given model | Cross-model fallback or the Workflow route, under a governor stream permit |
| Failed round | Ends the run | Retried up to twice when it failed before any text or tool call on a link error |
| Registry tool call (`ToolGate`) | Execute the tool and record file snapshots and stamps | Hooks, permissions, the full `ToolCtx`, file snapshots and stamps, output capping, steer notices |
| Unrun calls after a cancel | Failed as `cancelled` | Left open; the turn end closes them as `CANCELLED` |
| End | The round's finish | Also ends after an accepted `report` |

The kernel offers and handles the model's `rewrite` and `todo` calls in both
modes. The ladder's `ContextEvicted` events store the exact notice for each
evicted part, which the projection renders during replay.
`TurnBindingRecorded` records the runtime generation and the pre-hook
system prompt and tool list digests at turn start and whenever those
digests change. `StepStarted.view` fingerprints the request content after
`chat.params` and before provider routing.

## Rewrite contract

The contract a rewriter is written against lives in `hya-tool`
(`hya_tool::rewrite`), because a native bundle links only `hya-tool` and
`hya-proto`. All of it is part of the native ABI digest.

| Item | Implemented by | Purpose |
| --- | --- | --- |
| `Rewriter { before_round(cx), rewrite_now(cx) }` | the kernel's `HarnessRewriter` (bare mode); internal bundles | Rewrite before a round when needed, or now (`/summarize`); `Ok(true)` when the log changed. Stateless (`&self`) |
| `RewriteCtx` | the host, per call | Session, assistant message, round model and its `ModelWindow` (window, usage reporting, reasoning replay), projection, system-and-tools `overhead_tokens`, workdir, the rewriter's JSON `config`, and the services below |
| `SessionLog { projection, append }` | bare: `kernel::Scope` over the store; full mode: the turn's actor-claimed emit | Read the projection, append one event batch |
| `Transcript { messages(projection) }` | bare: the kernel agent's rendering; full mode: the agent's rendering with the round's prompt images (`/summarize`: the current episode) | The messages a round sends for a projection |
| `ModelHost { fold_agent, has_summarizer, summarize, compact_window }` | full mode only (`None` in bare) | The fixed agent that folds context (Compaction, or Summary for `/summarize`); the engine's summarizer, which renders the summary or handoff prompt and records the call's usage; route-native `/responses/compact` |
| `CompactionHooks { before, after }` | full mode only (`SessionEngine`) | The `compaction.before` (overflow) and `compaction.after` hook points |

The harness's `before_round` builds the context and calls the rewriter.
Full mode's rewriter is the first one the internal bundles register (today
`hya/compaction`, [Compaction](compaction.md)); `SessionEngine::new` loads
them once per process with `hya_core::bundles::load_internal()` and panics
when a listed bundle's library cannot be loaded, as a missing tool family
does. When no internal bundle registers a rewriter, full mode uses the
kernel's `HarnessRewriter`: the 80% rewrite of bare mode, with no ladder,
no `ContextStatus`, and `/summarize` removing everything before the
session's newest message. Full mode's `config` is the engine's
`CompactionConfig` plus the token-accounting mode, as JSON. A host failure
reaches the rewriter as a `ToolError`; the host keeps the original
`CoreError` and a failed rewrite ends the turn with it (for example a stale
actor claim or a missing fold agent).
Token estimation (`hya_tool::tokens`) is shared by host and bundles, and the
reasoning replay policy and compacted-context markers it and the request
encoders read are in `hya_proto::context`.
