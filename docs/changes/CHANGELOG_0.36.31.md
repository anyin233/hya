# 0.36.31

## All five oh-my-pi compaction mechanisms are built in, with a user-adjustable order (core, app)

- hya now ships every context-reduction mechanism oh-my-pi exposes under
  `compaction.methodOrder`, under the same names: `shake` (evict stale tool
  outputs to `artifact://` handles), `remote` (provider-native
  `/responses/compact`), `soft` (structured LLM summary of the folded prefix),
  `snapcompact` (a local, deterministic dense archive of the discarded
  history — no model call at all), and `handoff` (an LLM handoff document
  written over the verbatim transcript and committed as the compaction
  summary).
- The two new mechanisms make the ladder complete: a session with no
  summarizer wired — or a route whose model call fails mid-fold — can still
  compact, because `snapcompact` is entirely local; and a takeover can be
  handed a real handoff document instead of a prose summary. Both record
  their own `ContextCompacted` strategy on the wire (`snap_compact`,
  `handoff`).
- The order the mechanisms fire in is now user configuration, not a hardcoded
  ladder: `compaction.method_order` in `config.yaml` takes the oh-my-pi
  method names, and `HYA_COMPACTION_METHOD_ORDER` (comma-separated) wins over
  it. The walk stops at the first mechanism that fits; an unavailable one —
  an unsupported route, no summarizer — advances to the next. A partial list
  is completed with the unmentioned mechanisms in default order, and an
  unknown name ignores the whole value rather than silently reordering the
  ladder.
- The default order preserves hya's verified behavior exactly:
  `shake, remote, soft` first, then the new `snapcompact` and `handoff` as
  escalation. oh-my-pi's own default (`remote, snapcompact, handoff, shake,
  soft`) is one config line away.
- `snapcompact` archives use oh-my-pi's serialization budgets: tool results
  keep a 2,000-character head+tail around an explicit omission marker
  (verdicts and final state live at the ends of an output), tool-call
  arguments are capped per value and per call, and whitespace is collapsed —
  so one huge payload cannot crowd the turns around it out of the archive.
  oh-my-pi additionally renders the archive onto bitmap image frames for
  vision-capable models; hya commits the dense text itself, which every route
  can read (frame rendering remains a follow-up).
- A handoff call sends the transcript verbatim plus one trailing handoff
  prompt — not the rendered single-message serialization a summary gets — so
  the document describes where the session stands, including recent turns,
  and a cache-capable provider can reuse the live prefix.
- New canonical doc page `docs/compaction.md`: the mechanism set, walk
  semantics, order configuration, thresholds, wire records, and the explicit
  differences from oh-my-pi. `docs/architecture/runtime.md`'s compaction
  section was rewritten to match the current ladder (it still described the
  pre-0.36.29 two-tier path) and now defers to it.
