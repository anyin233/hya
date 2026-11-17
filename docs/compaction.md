# Compaction and Context Reduction

hya ships all five context-reduction mechanisms that oh-my-pi exposes under
`compaction.methodOrder`, under the same names, and lets you choose the order
they fire in. When a turn's transcript crosses the compaction threshold, the
engine walks your configured order and stops at the first mechanism that
brings the transcript back under the threshold; a mechanism that is
unavailable (an unsupported route, no summarizer wired) or fails advances to
the next one.

This page is the canonical reference for the mechanism set, the walk
semantics, the configuration surface, and the wire records each mechanism
emits. Thresholds and token accounting are configured alongside; see
[Configuration](configuration.md) for the full block.

## The five mechanisms

| Name (oh-my-pi) | What it does | Model call | Wire strategy |
| --- | --- | --- | --- |
| `shake` | Moves stale completed tool-output bodies to durable storage (`artifact://` handles), leaving the calls, inputs, and reasoning in place. Lossless: the body stays retrievable by reading the handle. | no | `ContextEvicted` |
| `remote` | Asks the route to fold its own context window (OpenAI Responses `/responses/compact`; `openai-response`, `openai-codex`, and `grok-build` routes). | provider-side | `native` |
| `soft` | Folds the transcript prefix into a structured, incrementally anchored summary (the fixed `hya-compaction` system agent). | yes | `local_summarizer` |
| `snapcompact` | Replaces the folded prefix with a local, deterministic dense archive under oh-my-pi's serialization budgets. Works with no summarizer wired at all. | no | `snap_compact` |
| `handoff` | Has a model write a handoff document over the **verbatim transcript** and commits it as the compaction summary. | yes | `handoff` |

Details worth knowing per mechanism:

- **`shake`** evicts a completed tool output when it lies outside the last
  `keep_recent` messages (older turns) or outside the last `keep_recent`
  completed tool steps. The step tail applies inside the active assistant
  message too, so one long turn sheds its early outputs mid-turn. Shake is
  sticky: `ContextEvicted` logs each evicted part with the exact notice that
  replaces its output, and the projection keeps that notice beside the part
  (`PartProjection::Tool.eviction_notice`), so every later request, in this
  process or after a restart, sends the same byte-stable notice and no second
  `ContextEvicted` is emitted for the part. Only newly encountered stale parts
  are added. The notice retains an appended `--- [NEW MAIL ...]` steer notice
  verbatim, so consumed mail remains visible to the model. With the session's
  artifact store the eviction is a *move*: the notice reads
  `[tool output moved to artifact://… ]` and `read` resolves the handle back to
  the full bytes. A body smaller than the notice replacing it is not spilled.
  The tool part keeps its full output in the log and the projection; clients
  see the full output, and only model requests send the notice.
- **`remote`** persists the provider's folded window behind the
  `HYA_COMPACTED_CONTEXT` marker; later Responses requests re-inject the items
  verbatim. If the native compact succeeds but the transcript is still over
  threshold, the walk escalates to the next mechanism instead of ending the
  sequence over the window.
- **`soft`** renders role, text, reasoning, tool name, bounded tool input and
  output (reasoning and output keep their head and tail, where conclusions
  and verdicts sit) into an eight-section template (primary intent → next
  step). When the
  session already carries a summary, it is passed as `<previous-summary>` so
  each compaction *updates* an anchored summary instead of re-paraphrasing
  one. Output is capped by `summary_max_tokens`.
- **`snapcompact`** is hya's model-free fold: serialization keeps a
  head+tail slice of every tool result (verdicts and final state live at the
  ends of an output, not its middle), caps tool-call arguments per value and
  per call, and collapses whitespace. The archive names how many messages it
  folded and is deterministic — the same transcript always produces the same
  archive. oh-my-pi renders the same archive onto bitmap image frames for
  vision-capable models; hya commits the dense text itself, which every route
  can read (see [differences from oh-my-pi](#differences-from-oh-my-pi)).
- **`handoff`** differs from `soft` in audience and input: the model reads the
  conversation exactly as it stands (recent turns included) plus one trailing
  handoff prompt, and writes a takeover document — goal, current state,
  files, decisions, errors, pending tasks, next step. The folded range still
  leaves the `keep_recent` tail verbatim, and the request shape matches the
  live conversation so a cache-capable provider can reuse the prefix.

## Default order and why

```text
shake → remote → soft → snapcompact → handoff
```

The default preserves hya's long-verified ladder exactly — the lossless,
spill (it moves outputs out of the request, not out of the log) runs before
anything that rewrites the transcript, and
the structured summary remains the primary fold — and adds the two new
mechanisms as escalation: `snapcompact` is the deterministic fallback that
works when no model call is available at all (no summarizer wired, or a
provider failure mid-fold), and `handoff` is the hardest fold, for handing a
session to a new agent rather than continuing it.

oh-my-pi's own default order is one configuration line away:

```yaml
compaction:
  method_order: [remote, snapcompact, handoff, shake, soft]
```

## Configuring the order

`compaction.method_order` in `config.yaml` takes the five names above; the
`HYA_COMPACTION_METHOD_ORDER` environment variable (comma-separated) wins
over the file value.

```yaml
compaction:
  # A partial list is completed with the unmentioned mechanisms in default
  # order; an unknown name ignores the whole value.
  method_order: [handoff, soft]
```

Resolution rules:

- **Partial lists are honored.** `[handoff, soft]` means handoff first, soft
  second, then the remaining mechanisms (`shake`, `remote`, `snapcompact`) in
  default order. The ladder can never *lose* a mechanism — unlike oh-my-pi's
  filter-and-drop, omission reorders, it does not disable.
- **Unknown names invalidate the value.** A typo keeps the previous value
  (the file order, or the engine default when the file named none) rather
  than silently reordering the ladder.
- **Duplicates collapse to first occurrence.**
- The engine-level invariant is a full permutation: whatever configuration
  resolves to, the turn loop always walks exactly five mechanisms, one of
  each kind.

The walk itself:

1. Before each mechanism, the loop re-checks the threshold — if the previous
   one already fit the transcript, nothing further runs.
2. An unavailable mechanism advances: `remote` on a route without a compact
   endpoint, `soft`/`handoff` when no summarizer is wired, `shake` when there
   is nothing left to evict.
3. A failed mechanism advances the same way: a native compact transport
   error or a summarizer failure falls through to the next rung, ending with
   the model-free `snapcompact` if the order leaves it last.
4. Every reduction is persisted or request-local exactly as before: folds
   (`remote`, `soft`, `snapcompact`, `handoff`) inject behind the
   `HYA_COMPACTED_CONTEXT` marker and drop pre-marker history on later
   requests. The marker is placed before the retained tail, including an
   in-flight assistant message, so the next round sees its prior tool calls
   and results. `shake` logs its notices in `ContextEvicted` and leaves the
   tool outputs in the log.

## In-flight turns

The marker is an event-sourced transcript boundary, not merely an appended
system message. `MessageStarted` is append-only, so the projection uses the
`ContextCompacted` folded range to place the marker before the retained tail.
This preserves the active assistant message across a mid-turn compaction; its
subsequent rounds continue with the tool calls and results already produced.
The compaction ladder excludes that marker from its foldable range, preventing
the same compacted boundary from being summarized repeatedly.

### Step folds

One assistant message holds every step of a turn. A long single turn, such as
a headless `hya exec` run (one prompt, one message with dozens of steps), has
no whole message the fold could take, so `soft`, `snapcompact` and `handoff`
also cut **inside** the in-flight message:

1. They first plan the usual message fold: everything after the previous
   marker except the last `keep_recent` messages (one more while the turn's
   assistant message is in flight).
2. When that folds nothing, or what it keeps is still over the threshold, and
   the in-flight message holds more than one step, they cut it between steps
   instead. Folded: the messages before the turn's head, and the oldest steps.
   Kept verbatim: the head (the prompt and system notices right before the
   message) and the newest `keep_recent` steps, fewer while those are over half
   the threshold, never fewer than one.
3. A step is cut whole: its reasoning, text, tool calls and results stay
   together, and a kept step goes out exactly as stored, its reasoning
   (`provider_data` included) byte for byte. The cut falls where a step's
   tool calls end and the next step begins, or, when no reasoning leads them,
   between two tool calls (a model that calls tools without reasoning or text
   in between).

The summarizer reads the folded messages, the head as context, and the folded
steps. A later step fold in the same turn anchors on the earlier summary, as
any repeated fold does.

The cut is recorded on `ContextCompacted.step_cut { through_part, kept_head }`:
`to_message` is the in-flight message, and its parts up to `through_part` are
folded. The projection places the marker before `kept_head` and counts the
folded parts on the message as `MessageProjection.folded_parts`; requests send
the parts after them. The projection keeps every part, so transcripts, revert,
and reports still see the whole message.

Example: a run `[prompt, HEADLESS RUN notice, assistant(65 steps)]` with
`keep_recent: 6` folds steps 1–59; the next request is
`[summary, prompt, notice, assistant(steps 60–65)]`, and the steps that follow
append to the same message.

## Thresholds

The walk trips when occupancy exceeds
`min(window * context_fraction, window - reserve_tokens)`, floored at 1,000
tokens; each rung then checks its own precondition (`shake` needs a stale tool
output, the folding rungs a foldable message prefix or, inside a turn, a
[step fold](#step-folds)). When the route advertises
no window, the flat `token_threshold` applies.
In `auto` mode, a reported provider measurement is anchored to the **latest
round** (`input + cache_read + cache_write`), then locally estimated content
appended after that round is added; cumulative usage from earlier rounds is not
summed. `ContextStatus.source` is `provider` when this measurement is accepted.
Estimates follow the route encoder: Google omits reasoning, Chat OpenAI
resends chat-native `reasoning_content` from every turn, Responses resends
only a reasoning part's provider data, and Anthropic replays only signed
current-turn thinking. Encoder and estimator share one predicate
(`hya_provider::ReasoningReplayPolicy::replays`), so they cannot drift.
Tool outputs appended to the running assistant message after its latest round
are not yet in that round's measurement; the next round's report includes them.
When the local preflight estimate has already crossed `window` even though the
latest provider report still sits below the threshold, the ladder falls back to
the local estimate and reduces the request before sending so preflight does not
reject an un-compacted turn.
See [Configuration](configuration.md) for every field and its environment override.

## Observability

Each mechanism records what it did on the event log:

- `ContextCompacted` — one per fold, with the `strategy` that produced it
  (`native`, `local_summarizer`, `snap_compact`, `handoff`, `rewrite`), the folded range
  (`from_message..=to_message`, `folded_count`), the estimated input tokens
  that tripped the threshold, and the threshold in force. A
  [step fold](#step-folds) adds `step_cut { through_part, kept_head }`, and
  `folded_count` then counts the cut message as one. The folded input is a
  pointer, not a copy: the range plus the event log reconstructs exactly what
  was folded.
- `ContextEvicted` — recorded whenever `shake` saved tokens, including when
  the saving alone was not enough and the walk escalated anyway. Its payload
  contains `session`, `evicted_parts`, `parts`, `tokens_before`,
  `tokens_after`, and `threshold`. `parts` lists `{ part, notice }` for each
  evicted tool part: `notice` is the exact text requests send instead of the
  output, artifact handle included, so the log alone reproduces the request.
  Logs written before `parts` decode it as empty.
- `ContextStatus` — emitted once per streaming round after the ladder, with
  the occupancy the request actually carries, its source (provider-reported or
  estimated), the accounting mode, and the resolved threshold. Clients surface
  this report, for example in a context-usage panel.

## Differences from oh-my-pi

| Aspect | oh-my-pi | hya |
| --- | --- | --- |
| Method set | `remote`, `snapcompact`, `handoff`, `shake`, `soft` | identical, same names |
| Default order | `remote, snapcompact, handoff, shake, soft` | `shake, remote, soft, snapcompact, handoff` (preserves hya's verified spill-first ladder; omp's order is one config line) |
| `snapcompact` payload | dense archive rendered onto per-model bitmap image frames for vision-capable routes | dense text archive with the same serialization budgets; every route reads it, no vision capability required |
| Omitted methods | dropped from the method list (can disable a method) | re-ordered, never dropped — the ladder always carries all five |
| Remote lanes | Responses compact V1/V2 streaming, Anthropic server-side compaction beta, custom endpoint | Responses-family `/responses/compact` (`openai-response`, `openai-codex`, `grok-build`) |

Bitmap-frame rendering for `snapcompact` is a possible follow-up: the
mechanism's essence — a local, deterministic, model-free archival pass under
the omp budgets — is what the ladder relies on, and that is what ships.

## Source map

| Concern | Location |
| --- | --- |
| Rungs, wire names, order parsing, snapcompact archive, handoff plan | [`crates/hya-engine/src/compaction.rs`](../crates/hya-engine/src/compaction.rs) |
| Ladder walk in the turn loop | [`crates/hya-engine/src/engine/turn.rs`](../crates/hya-engine/src/engine/turn.rs) |
| Config file/env resolution | [`crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs) |
| Wire events and strategies | [`crates/hya-proto/src/event.rs`](../crates/hya-proto/src/event.rs) |
| Artifact spill sink | [`crates/hya-engine/src/engine/spill.rs`](../crates/hya-engine/src/engine/spill.rs), [`crates/hya-tool/src/handle/`](../crates/hya-tool/src/handle/) |

Marker publication is atomic: each persisted reduction writes its completed
system marker and `ContextCompacted` range event in one transaction, then
publishes the committed events. Persistence and actor-fence errors fail the
turn; they do not cause the ladder to try another method.

Manual `/compact` (`SessionEngine::summarize_session(SessionId) ->
Result<MessageId, CoreError>`) acquires the session turn lease before reading
history and holds it through the atomic marker commit. For example, requesting
`/compact` during a running turn queues until that turn releases its lease.
Both manual summaries and terminal handoffs preserve tool names, arguments,
results and errors through the same part conversion used by automatic requests.

`SessionEngine::terminal_handoff(SessionId) -> Result<TerminalHandoff, CoreError>`
also queues for the turn lease; admission errors propagate, while summarizer
failures retain the degraded-document contract. Resident archival holds its
lease through generation and handoff publication and uses the internal
already-claimed entry point. Gate draining cancels queued admission.
