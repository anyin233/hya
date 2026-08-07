# 0.35.0

- **Breaking: an `AgentBundle` now defines exactly one agent.** The manifest key
  `agents:` (a list) becomes `agent:` (a map), `local_id` + `stable_id` collapse
  to a single `id`, and `api_version` and `harness_access` are removed. A
  manifest that still carries any of them is rejected by name. **Every installed
  bundle must be reinstalled after upgrading**; a row written by an older binary
  is skipped with a warning and shown as `unreadable (reinstall)` by
  `hya bundle list`.
- **A bundle agent's tool plane is host-controlled.** It is derived from the
  agent's origin instead of declared by the author: an installed bundle agent
  sees the internal public tool snapshot plus its own bundle resources, and
  never a tool installed at the main-agent level, a tool installed into hya
  directly, an MCP server's exports, or a project or user skill. Note that this
  bounds direct use only — a bundle agent may still spawn a built-in, which runs
  on its own full plane.
- Fixed a cross-bundle leak: a bundle could name `bundle:<other>/tool/x` in its
  `resource_view` and pull in another bundle's tool. Such a reference is now
  refused.
- Built-in agents left the bundle system. They are compiled into the binary, so
  `bundles/builtin/` and the `hya-app` build-time prepare step are gone, their
  ids are reserved against installed bundles, and an ordinary built-in can spawn
  any installed bundle agent without a configuration edit.
- **The team mailbox is scoped to the hierarchy.** An agent can address its
  parent, its siblings, and its direct reports; a cross-unit send is refused.
  Channel keys are unit-qualified and `AgentRegistered` carries the parent path.
  See ADR 0011.

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

- The event log is now a sufficient statistic for offline reconstruction of a
  session's agent call graph, each agent's trajectory, and every compaction. An
  offline viewer can rebuild all three from the store alone. No new tables and no
  migrations: every addition rides existing `event_log` rows.
- Added `Event::ContextCompacted`, recorded on the log of the session that
  compacted. It carries the strategy used (provider-native vs local summarizer),
  the folded range as `from_message`/`to_message` plus `folded_count`, the token
  estimate that tripped the threshold, and the threshold in force. The folded
  input is a pointer, not a copy — compaction never deletes, so the range plus
  the log reconstructs exactly what the summarizer saw.
- Local-summarizer compaction now persists its result behind the same
  `HYA_COMPACTED_CONTEXT` marker the native path already used. Previously the
  summary died with the request, so every later round re-summarized the same
  history. **This changes model input on routes without native compaction:**
  later rounds now slice from the marker instead of replaying full history.
- `Event::MemberSpawned` now carries the parent's `directive` verbatim and the
  `tool_call` that produced the spawn. The directive is the subagent's purpose;
  the child's first user message was not a reliable substitute, because a resumed
  session receives the directive as a later message and a resident agent also
  receives mail as user prompts. Summarizing a purpose stays an offline concern.
- Added `Event::SessionForked`, recording a fork's source session and cut point.
  Forks previously left no durable trace at all — `parent` is deliberately unset
  (it means subagent lineage) and copied messages get fresh ids — so a forked
  session appeared as an orphan root in any reconstructed graph.
- Added `Message::id()` and `hya_core::plan_compaction` / `CompactionPlan`.
  `compact_with` is unchanged and now wraps `plan_compaction`.
- All additions are additive and `#[serde(default)]`: logs written before this
  release still replay unchanged, and older binaries fold the new variants
  through the existing `Event::Unknown` path.
