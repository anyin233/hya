# 0.35.1

First release since v0.35.0 was cut internally; it also carries the unreleased
0.34.10–0.34.14 development versions. Intermediate notes are in `docs/changes/`.

## Breaking

- **An `AgentBundle` now defines exactly one agent.** The manifest key `agents:`
  (a list) becomes `agent:` (a map), `local_id` + `stable_id` collapse to a
  single `id`, and `api_version` and `harness_access` are removed. A manifest
  that still carries any of them is rejected by name. **Every installed bundle
  must be reinstalled after upgrading**; a row written by an older binary is
  skipped with a warning and shown as `unreadable (reinstall)` by
  `hya bundle list`.
- **A bundle agent's tool plane is host-controlled.** It is derived from the
  agent's origin instead of declared by the author: an installed bundle agent
  sees the internal public tool snapshot plus its own bundle resources, and never
  a tool installed at the main-agent level, a tool installed into hya directly,
  an MCP server's exports, or a project or user skill. This bounds direct use
  only — a bundle agent may still spawn a built-in, which runs on its own full
  plane.
- **The team mailbox is scoped to the hierarchy.** An agent can address its
  parent, its siblings, and its direct reports; a cross-unit send is refused.
  Channel keys are unit-qualified and `AgentRegistered` carries the parent path.
  See ADR 0012.
- **Local-summarizer compaction is now sticky.** Its summary persists behind the
  same `HYA_COMPACTED_CONTEXT` marker the provider-native path already used.
  Previously the summary died with the request, so every later round
  re-summarized the same history. On routes without native compaction, later
  rounds now slice from the marker instead of replaying full history.

## Bundles and agents

- Fixed a cross-bundle leak: a bundle could name `bundle:<other>/tool/x` in its
  `resource_view` and pull in another bundle's tool. Such a reference is refused.
- Built-in agents left the bundle system. They are compiled into the binary, so
  `bundles/builtin/` and the `hya-app` build-time prepare step are gone, their
  ids are reserved against installed bundles, and an ordinary built-in can spawn
  any installed bundle agent without a configuration edit.

## Context observability

The event log is now a sufficient statistic for offline reconstruction of a
session's agent call graph, each agent's trajectory, and every compaction. No new
tables and no migrations: every addition rides existing `event_log` rows.

- `Event::ContextCompacted` records each compaction on the log of the session
  that compacted: the strategy used (provider-native vs local summarizer), the
  folded range as `from_message`/`to_message` plus `folded_count`, the token
  estimate that tripped the threshold, and the threshold in force. The folded
  input is a pointer, not a copy — compaction never deletes, so the range plus
  the log reconstructs exactly what the summarizer saw.
- `Event::MemberSpawned` now carries the parent's `directive` verbatim and the
  `tool_call` that produced the spawn. The directive is the subagent's purpose;
  the child's first user message was not a reliable substitute, because a resumed
  session receives the directive as a later message and a resident agent also
  receives mail as user prompts.
- `Event::SessionForked` records a fork's source session and cut point. Forks
  previously left no durable trace at all — `parent` is deliberately unset (it
  means subagent lineage) and copied messages get fresh ids — so a forked session
  appeared as an orphan root in any reconstructed graph.
- Added `Message::id()`, `hya_core::plan_compaction`, and `CompactionPlan`.
- All additions are additive and `#[serde(default)]`: logs written before this
  release still replay unchanged, and older binaries fold the new variants
  through the existing `Event::Unknown` path.

## Context efficiency

- Compaction scales to the model instead of a fixed guess. The threshold is a
  fraction of the route's advertised `max_context` (default `0.75`, override with
  `HYA_COMPACTION_CONTEXT_FRACTION`). A route that advertises no window keeps the
  previous flat `HYA_COMPACTION_THRESHOLD` behaviour, and a nonsense fraction
  falls back rather than being trusted. Resolved thresholds are clamped to a
  floor so a tiny window cannot produce a compact-every-turn loop.
- Compaction decisions use **provider-reported token counts** where they exist.
  The most recent assistant message's usage gives the exact prompt size the
  provider counted; only messages appended after it are estimated. `chars / 4`
  remains the fallback for routes that never report usage, so their behaviour is
  unchanged. Window occupancy counts `input + cache_read`, which can only
  over-count — failing safe by compacting early rather than overflowing.
- **Stale tool outputs are evicted before anything is summarized.** Tool payloads
  dominate a tool-heavy transcript, and dropping an old result costs the model far
  less than folding whole turns into prose: every tool call, its input, and all
  reasoning survive, and the placeholder tells the model it can re-run the tool.
  When eviction alone brings the transcript under the threshold, no summarizer
  call is made at all. Recorded as `Event::ContextEvicted`. Eviction is
  request-local: the event log still holds every tool output in full, so the
  offline-reconstruction guarantee above is preserved.
- `AGENTS.md` discovery is cached per workdir chain, validated by each file's
  modified-time and length, with the cheap directory walk re-run every call so a
  newly added `AGENTS.md` still invalidates.
- New public API: `ProviderRouter::capabilities`, and in `hya-core`
  `resolved_threshold`, `measured_tokens`, `tokens_in_use`, `needs_compaction_at`,
  `plan_compaction_at`, `fold_prefix`, and `evict_stale_tool_outputs`.
  `CompactionConfig` gains `context_fraction`. `needs_compaction`,
  `plan_compaction`, and `compact_with` keep their previous behaviour.

## Fixes

- Fixed the bundle process-E2E suite, failing since the one-agent-per-bundle
  change. That commit regenerated the public bundle fixture and renamed its ids
  (`hya/public-fixture` → `hya/valid-public`, `public-fixture-lead` →
  `valid-public-lead`), updating the backend and bundle-crate tests but not
  `crates/hya-e2e/tests/p11_hyabundle.rs`. The gap survived because `hya-e2e` is
  excluded from the default workspace test run.
- `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --all --check` pass again. `hya-sdk`, `hya-native`, and the `hya`
  integration tests used `unwrap`/`expect` in test code without the
  `#![allow(clippy::unwrap_used, clippy::expect_used)]` the rest of the workspace
  applies to its test modules. The allow is scoped to test code only — library
  and binary paths still deny both lints.
- `hya-sdk`'s `native_spike` example reports a missing `HYA_BACKEND_DIR` through
  its `Result` instead of panicking, keeping the same operator-facing message.
