# ADR-0033: A three-primitive kernel in `hya-core`; everything else is a bundle

Date: 2026-11-16
Status: Accepted

## Context

An agent's work is editing its Context. The edits have exactly three sources:
deterministic observations of the outside world (tool calls, and user input,
which enters through the same channel), the model's own sampled text and
judgement, and a destructive, non-append-only edit that frees window space.

`hya-core` today is the whole runtime (about 61k lines), and the Context it
sends to the model is implicit:

- The request is recomputed every round across about 600 lines of
  `run_turn_rounds`: projection, message mapping, sticky evictions, the
  compaction ladder, system prompt and tool schemas, then the `chat.params`
  hook. No single place answers "what is the Context now".
- Some rewrites never reach the event log. Sticky evictions live only in
  process memory, and the per-round rebinding replaces the system prompt and
  tool set without a record, so the same log can render different requests in
  different processes.
- Some "tool results" do not come from a tool: steer notices are appended to
  another tool's output, and an audited path returned a synthesized success
  without running the tool.
- `hya_tool::ToolCtx` carries twenty runtime planes, while a minimal tool call
  needs a name, a schema, input, a working directory and a cancel signal.

## Decision

### Decisions

- **D1** The three primitives are the three sources of Context tokens:
  **tool append** (deterministic outside observation), **model append** (the
  model's own text and judgement), and **rewrite** (non-append-only,
  destructive). User input uses the tool-append channel.
- **D2** The kernel replaces `hya-core`'s inside in place. No parallel crate,
  no rename. Unrelated parts move out to bundles.
- **D3** The kernel ships `bash`, `read`, `write`, `edit`, without permission
  checks.
- **D4** Both the harness and the model can start a rewrite; the kernel
  offers a `rewrite` tool.
- **D5** Moved-out features become **internal bundles**: `.hyabundle`
  packages loaded in full mode.
- **D6** The kernel has a minimal system prompt modelled on pi's.
- **D7** A model-started rewrite cannot replace user input.
- **D8** The SQLite event log keeps its existing logic and only grows. A
  rewrite leaves one event and replaces no earlier record. The live Context
  holds only a short system prompt, the tool list, and the live work.
- **D9** Loading `.hyabundle` packages and writing the SQLite event log
  belong to core and stay present in bare mode.
- **D10** Core offers no way to read removed text back.
- **D11** Native bundles may register hooks; permission and other intercepting
  features use the existing hook contract.

### Invariants

1. **Inference depends only on the Context.** The live Context is the event
   log folded by the `hya-proto` reducer. The request is a pure function of
   it: after a restart the log yields a byte-identical request.
2. **The Context changes only through the three primitives, and each change
   is an event in the log.** The log only grows.
3. **Observations have provenance and cannot be forged.** Only tool append
   brings in outside facts, and every tool result pairs with a call the model
   made. The harness never synthesizes a tool result. Unknown tools, invalid
   input, execution errors and cancellation all return to the Context as
   failed tool results. Provenance is the event type.
4. **Rewrite only removes; it never poses as an observation.** It works on
   whole steps, leaves no dangling call or result, never strips reasoning on
   its own, keeps the text of user input, and leaves the original in the log.

### Primitives mapped onto existing events

No new event types.

| Primitive | Events |
| --- | --- |
| model append | `StepStarted`, `Text*`, `Reasoning*`, `ToolCallRequested`, `StepFinished`, `UsageRecorded`, inside an assistant `MessageStarted`/`MessageFinished` |
| tool append (tool) | `ToolResult`, `ToolError` |
| tool append (user) | user `MessageStarted`, `Text*`, `MessageFinished` |
| rewrite | a `role: System` marker message plus `ContextCompacted{strategy: Rewrite, step_cut}` |

The one wire addition is the `CompactionStrategy::Rewrite` variant (`rewrite`).

### Rewrite rules

A rewrite removes **everything before the current step**: all earlier
messages and the earlier steps of the current assistant message. It keeps the
current turn's user message (`StepCut.kept_head`) and the current step. It
emits, in one batch, a system marker message whose text says who removed the
context, that it stays in the log and cannot be read back, and repeats every
removed user message verbatim; then `ContextCompacted{strategy: Rewrite}`.

The model's `rewrite` tool takes one argument, `summary`. The summary is the
call's own input and stays in the Context as the model's text; the tool result
says so (`"The summary in this call is your own text, not an observation."`).
With nothing to remove, the tool returns the failed result
`nothing to remove` and no `ContextCompacted`.

The harness starts a rewrite before a round when the last round's
`prompt + output` tokens exceed 80% of the model's `max_context`. If either
number is missing it does nothing, and a provider error for an oversized
request surfaces unchanged.

### What core is

`hya-core` = the kernel (`hya_core::kernel`) + `.hyabundle` loading + event
log writing. Its end-state workspace dependencies are `hya-proto`,
`hya-provider`, `hya-store`, `hya-bundle` and `hya-tool`. `hya-tool` stays
because it is the ABI crate native bundles link (`Tool`, `ToolCtx`, the
native loader); moving the trait into core would make every cdylib link
`hya-store` and sqlx.

## Consequences

- **Bare mode** (`hya exec --bare`) loads no bundle: six tools, the minimal
  prompt, the 80% harness rewrite, the event log. No permissions, hooks,
  subagents, Workflows, compaction ladder, output truncation, or path
  boundaries; those belong to bundles. `bash` keeps one limit of its own, a
  per-call timeout (default 300 s): without it a single command can consume
  a whole run before the model gets the turn back.
- Image attachments of removed user messages are not kept; only their text is
  repeated in the marker.
- `CompactionStrategy::Rewrite` is a wire-format addition; older readers that
  match the enum exhaustively must be updated together.
- Invariant 1 is relaxed in one place: the full text of the system prompt and
  tool list is not logged. `TurnBindingRecorded` records their SHA-256 digests.
  "Same request after restart" holds as long as the bundle versions are
  unchanged.
- ADR-0012's host-controlled tool plane does not apply in bare mode: the
  kernel's six tools are the whole plane.

## Alternatives rejected

1. **New Context, entry or provenance types.** The event log plus the reducer
   already fold an append-only log into the live Context, and the event type
   already states provenance.
2. **A new rewrite event.** `ContextCompacted` plus a marker message already
   records the range and step cut without touching earlier events; only a
   strategy variant is added.
3. **A kernel-specific `Tool` trait.** Native bundles link `hya_tool::Tool`;
   `ToolCtx` planes have ready `disconnected()`/`default()` constructors that
   bare mode uses.
4. **A separate interceptor extension point.** The existing hook contract
   covers it, and native bundles can register hooks.
5. **A `rewrite` tool with range arguments, artifact entries, or a "must be
   the only call" rule.** One argument, `summary`; the range is fixed to
   "everything before this call"; the summary is the call input, so its
   provenance is honestly the model.
6. **Extractive or batched harness-side folding.** The kernel only
   guarantees that the request can be sent; better folding belongs in
   `hya/compaction`.
7. **Logging the full system prompt and tool list.** Digests are logged; the
   text is determined by bundle versions.
8. **An example entry point inside `hya-core`.** Real provider configuration
   lives in `hya-app`, out of `hya-core`'s reach; the entry point is the final
   form, `hya exec --bare`.
