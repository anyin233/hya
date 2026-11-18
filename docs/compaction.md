# Compaction and Context Reduction

The kernel decides when the Context is reduced, in bare and full mode alike.
Before each round it measures the request against 80% of the model's window.
Over that threshold it first runs its extensions, then, when the request is
still over, its own [Rewrite](kernel.md#rewrite-rules), which removes
everything before the current step behind a marker that repeats the removed
user messages verbatim.

Full mode has one extension, the `hya/compaction` internal bundle
(`bundles/internal/compaction`), a native library full mode loads at startup
(see [Rewrite contract](kernel.md#rewrite-contract)). It contributes two
steps that are cheaper or more faithful than the kernel's Rewrite:

| Step | What it does | Model call | Wire record |
| --- | --- | --- | --- |
| `shake` | Moves stale completed tool-output bodies to durable storage (`artifact://` handles), leaving the calls, inputs, and reasoning in place. Lossless: the body stays retrievable by reading the handle. | no | `ContextEvicted` |
| `remote` | Asks the route to fold its own context window (OpenAI Responses `/responses/compact`; `openai-response`, `openai-codex`, and `grok-build` routes). | provider-side | `ContextCompacted { strategy: native }` |

`hya exec --bare` loads no bundle, so its reduction is the kernel's Rewrite
alone.

## Before each round

1. **Pressure.** The kernel computes
   `used = max(last round's prompt + output, usage-anchored count of the request, local estimate of the request)`
   (the request counts include the system prompt and offered tool schemas)
   and `threshold = max_context * 8 / 10`. A route that advertises no window
   gives no pressure: nothing is reduced. The request counts include tool
   results that arrived after the last round, and the local estimate is what
   full mode's pre-send check measures, so a request that would fail that
   check is reduced first.
2. **Extensions.** The kernel calls each extension in registration order
   with the pressure. Extensions run every round; they reduce only when
   `used > threshold`. The `hya/compaction` bundle runs `shake`, then
   `remote` when the request is still over the threshold, and records the
   round's `ContextStatus`.
3. **Re-measure.** When an extension changed the log, the kernel re-reads
   the projection and estimates the request again from it; the recorded
   usage described a prompt that no longer exists.
4. **Kernel Rewrite.** Still over the threshold, the kernel's own Rewrite
   runs on the current projection.
5. **Goal checkpoint.** When nothing changed in steps 2 to 4, the kernel may
   append its [goal checkpoint](kernel.md#session-todos).

The model's `rewrite` tool call is kernel-only: no extension runs for it.

Example: a 200k window gives a 160,000-token threshold. A turn whose request
reaches 170,000 tokens first spills the stale tool outputs outside the kept
tail. If that brings it to 120,000, the round is sent. If it is still at
165,000 and the route has native compaction, the route folds the window; on
any other route the kernel removes everything before the current step.

## `shake`

`shake` evicts a completed tool output when it lies outside the last
`keep_recent` messages (older turns) or outside the last `keep_recent`
completed tool steps. The step tail applies inside the active assistant
message too, so one long turn sheds its early outputs mid-turn. It leaves the
newest visible `todo` result in place.

Shake is sticky: `ContextEvicted` logs each evicted part with the exact
notice that replaces its output, and the projection keeps that notice beside
the part (`PartProjection::Tool.eviction_notice`), so every later request, in
this process or after a restart, sends the same byte-stable notice and no
second `ContextEvicted` is emitted for the part. Only newly encountered stale
parts are added. The notice retains an appended `--- [NEW MAIL ...]` steer
notice verbatim, so consumed mail remains visible to the model. With the
session's artifact store the eviction is a *move*: the notice reads
`[tool output moved to artifact://… ]` and `read` resolves the handle back to
the full bytes. A body smaller than the notice replacing it is not spilled.
The tool part keeps its full output in the log and the projection; clients
see the full output, and only model requests send the notice.

## `remote`

`remote` sends the transcript to the route's `/responses/compact` on the
round's model and persists the returned window behind the
`HYA_COMPACTED_CONTEXT` marker, followed by the rendered
[session todo list](kernel.md#session-todos) when it has items. Later
Responses requests re-inject the items verbatim and then send the carried
list as a system item. A route without a compact endpoint, or a failed call,
leaves the reduction to the kernel's Rewrite.

## `/summarize` and `/compact`

`SessionEngine::summarize_session(SessionId) -> Result<MessageId, CoreError>`
(the v1 `SummarizeSession` and `CompactSession` rpcs, and the TUI's
`/summarize`) reduces the session's current episode now. Each extension's
`rewrite_now` runs in order and the first that changes the log ends it; the
`hya/compaction` bundle tries `remote` only. Otherwise the kernel's Rewrite
runs, anchored on the session's newest assistant message: it keeps that
turn's user message and removes the rest. No model writes a summary. The
call returns the marker's message id, and fails with `nothing to summarize`
when the session has no assistant message or nothing left to remove. The
fold records `threshold: 0`, which marks it manual (`CompactionApplied.manual`
on the v1 stream); `CompactSession` answers with the recorded strategy,
`native` or `rewrite`.

It acquires the session turn lease before reading history and holds it
through the atomic marker commit, so a request during a running turn queues
until that turn releases its lease.

## Configuration

```yaml
compaction:
  keep_recent: 6            # HYA_COMPACTION_KEEP_RECENT
  summary_max_tokens: 4096  # HYA_COMPACTION_SUMMARY_MAX_TOKENS (terminal handoff only)
  token_accounting: auto    # HYA_TOKEN_ACCOUNTING: auto | provider | estimate
```

`keep_recent` is the tail `shake` keeps. `token_accounting` selects how
`ContextStatus` measures occupancy; the kernel's trigger does not depend on
it. The threshold is fixed at 80% of the window. See
[Configuration](configuration.md) for the full block.

## Observability

- `ContextCompacted`: one per fold, with its `strategy` (`native` for
  `remote`, `rewrite` for the kernel), the folded range
  (`from_message..=to_message`, `folded_count`), the tokens that tripped the
  threshold (`input_tokens_est`), and the threshold in force. A kernel
  Rewrite inside the running message adds
  `step_cut { through_part, kept_head }`. Logs also carry the strategies of
  removed reductions (`local_summarizer`, `snap_compact`, `handoff`); they
  replay unchanged.
- `ContextEvicted`: recorded whenever `shake` saved tokens, including when
  the kernel rewrote afterwards. Its payload contains `session`,
  `evicted_parts`, `parts`, `tokens_before`, `tokens_after`, and
  `threshold`. `parts` lists `{ part, notice }` for each evicted tool part:
  `notice` is the exact text requests send instead of the output, so the log
  alone reproduces the request. Logs written before `parts` decode it as
  empty.
- `ContextStatus`: emitted by the `hya/compaction` bundle once per round,
  after its steps, with the occupancy the request carries in the configured
  accounting mode, its source (provider-reported or estimated), and the
  kernel's threshold (`0` without a window). A kernel Rewrite that follows is
  reported by its own `ContextCompacted`.

Each persisted fold writes its completed system marker and `ContextCompacted`
in one transaction, then publishes the committed events. Persistence and
actor-fence errors fail the turn.

## Hooks

The kernel dispatches `compaction.before` (trigger `overflow`, with the
tokens in use) once per round before its extensions when the request is over
the threshold, and `compaction.after` once its own Rewrite committed;
`remote` dispatches `compaction.after` when it committed a fold. The reply of
`compaction.before` is ignored: an overflow cannot be skipped, and no
summarizer prompt exists to replace. See
[Plugin protocol](plugin-protocol.md).

## Terminal handoff

`SessionEngine::terminal_handoff(SessionId) -> Result<TerminalHandoff, CoreError>`
writes the state document an archived subagent carries into its next episode
(ADR-0015). It is the only user of the engine's summarizer: the model reads
the ended episode verbatim plus one trailing state-handoff prompt, capped by
`summary_max_tokens`, and the call is billed to the session as
`compaction` usage. It queues for the turn lease; admission errors propagate,
while summarizer failures produce the degraded document.

## Source map

| Concern | Location |
| --- | --- |
| Pressure, extension order, kernel Rewrite, `/summarize` order | [`crates/hya-core/src/kernel/rewrite.rs`](../crates/hya-core/src/kernel/rewrite.rs) |
| `shake`, `remote`, `ContextStatus` | [`bundles/internal/compaction/native/src/`](../bundles/internal/compaction/native/src/) (`ladder.rs`, `evict.rs`, `config.rs`) |
| Rewrite context, native compaction and hook services, error mapping | [`crates/hya-engine/src/engine/turn/rewrite_host.rs`](../crates/hya-engine/src/engine/turn/rewrite_host.rs), [`harness.rs`](../crates/hya-engine/src/engine/turn/harness.rs), [`summary.rs`](../crates/hya-engine/src/engine/summary.rs) |
| Settings, terminal handoff summarizer | [`crates/hya-engine/src/compaction.rs`](../crates/hya-engine/src/compaction.rs) |
| Config file/env resolution | [`crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs) |
| Wire events and strategies | [`crates/hya-proto/src/event.rs`](../crates/hya-proto/src/event.rs) |
