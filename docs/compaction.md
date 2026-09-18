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
| `soft` | Folds the transcript prefix into a structured, incrementally anchored summary (the fixed `compaction` system agent). | yes | `local_summarizer` |
| `snapcompact` | Replaces the folded prefix with a local, deterministic dense archive under oh-my-pi's serialization budgets. Works with no summarizer wired at all. | no | `snap_compact` |
| `handoff` | Has a model write a handoff document over the **verbatim transcript** and commits it as the compaction summary. | yes | `handoff` |

Details worth knowing per mechanism:

- **`shake`** evicts only completed tool outputs older than `keep_recent`
  messages. With the session's artifact store the eviction is a *move*: the
  transcript keeps `[tool output moved to artifact://… ]` and `read` resolves
  the handle back to the full bytes. A body smaller than the notice replacing
  it is not spilled. Idempotent: notices are recognized and never re-evicted.
- **`remote`** persists the provider's folded window behind the
  `HYA_COMPACTED_CONTEXT` marker; later Responses requests re-inject the items
  verbatim. If the native compact succeeds but the transcript is still over
  threshold, the walk escalates to the next mechanism instead of ending the
  sequence over the window.
- **`soft`** renders role, text, reasoning, tool name, bounded tool input and
  output into an eight-section template (primary intent → next step). When the
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
request-local spill runs before anything that rewrites the transcript, and
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
   requests; `shake` is request-local and never touches the event log.

## Thresholds

The walk trips when `messages.len() > keep_recent` and occupancy exceeds
`min(window * context_fraction, window - reserve_tokens)`, floored at 1,000
tokens. When the route advertises no window, the flat `token_threshold`
applies. Occupancy is measured by the token-accounting mode
(`auto` / `provider` / `estimate`). See
[Configuration](configuration.md) for every field and its
environment override.

## Observability

Each mechanism records what it did on the event log:

- `ContextCompacted` — one per fold, with the `strategy` that produced it
  (`native`, `local_summarizer`, `snap_compact`, `handoff`), the folded range
  (`from_message..=to_message`, `folded_count`), the estimated input tokens
  that tripped the threshold, and the threshold in force. The folded input is
  a pointer, not a copy: the range plus the event log reconstructs exactly
  what was folded.
- `ContextEvicted` — recorded whenever `shake` saved tokens, including when
  the saving alone was not enough and the walk escalated anyway.
- `ContextStatus` — emitted once per streaming round after the ladder, with
  the occupancy the request actually carries, its source
  (provider-reported or estimated), the accounting mode, and the resolved
  threshold. The TUI sidebar's Context panel renders this report.

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
| Rungs, wire names, order parsing, snapcompact archive, handoff plan | [`crates/hya-core/src/compaction.rs`](../crates/hya-core/src/compaction.rs) |
| Ladder walk in the turn loop | [`crates/hya-core/src/engine/turn.rs`](../crates/hya-core/src/engine/turn.rs) |
| Config file/env resolution | [`crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs) |
| Wire events and strategies | [`crates/hya-proto/src/event.rs`](../crates/hya-proto/src/event.rs) |
| Artifact spill sink | [`crates/hya-core/src/engine/spill.rs`](../crates/hya-core/src/engine/spill.rs), [`crates/hya-tool/src/handle/`](../crates/hya-tool/src/handle/) |
