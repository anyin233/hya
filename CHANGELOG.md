# 0.34.15

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
