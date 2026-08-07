# 0.34.16

- Compaction now scales to the model instead of a fixed guess. The threshold is a
  fraction of the route's advertised `max_context` (default `0.75`, override with
  `HYA_COMPACTION_CONTEXT_FRACTION`). `Capabilities::max_context` was already
  known but only ever reported to the catalog API; it now drives the decision.
  A route that advertises no window keeps the previous flat
  `HYA_COMPACTION_THRESHOLD` behaviour, and a nonsense fraction falls back rather
  than being trusted. Resolved thresholds are clamped to a floor so a tiny window
  cannot produce a compact-every-turn loop.
- Compaction decisions now use **provider-reported token counts** where they
  exist. The most recent assistant message's usage gives the exact prompt size the
  provider counted; only messages appended after it are estimated. `chars / 4`
  remains the fallback for routes that never report usage, so their behaviour is
  unchanged. Window occupancy counts `input + cache_read`, which can only
  over-count — failing safe by compacting slightly early rather than overflowing.
- **Stale tool outputs are now evicted before anything is summarized.** Tool
  payloads dominate a tool-heavy transcript, and dropping an old result costs the
  model far less than folding whole turns into prose: every tool call, its input,
  and all reasoning survive, and the placeholder tells the model it can re-run the
  tool. When eviction alone brings the transcript under the threshold, no
  summarizer call is made at all. Recorded as the new additive
  `Event::ContextEvicted`.
  Eviction is **request-local**: the event log still holds every tool output in
  full, so the guarantee from 0.34.15 — that the log is a sufficient statistic for
  offline reconstruction — is preserved.
- `AGENTS.md` discovery is cached per workdir chain, validated by each file's
  modified-time and length, with the cheap directory walk re-run every call so a
  newly added `AGENTS.md` still invalidates. Subagents already inherited the
  rendered guidance and never re-walked the filesystem, so this removes repeated
  reads per top-level turn rather than any token duplication.
- New public API: `ProviderRouter::capabilities`, and in `hya-core`
  `resolved_threshold`, `measured_tokens`, `tokens_in_use`, `needs_compaction_at`,
  `plan_compaction_at`, `fold_prefix`, and `evict_stale_tool_outputs`.
  `CompactionConfig` gains `context_fraction`. `needs_compaction`,
  `plan_compaction`, and `compact_with` keep their previous behaviour.
