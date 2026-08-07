# Design — Context observability

## 1. Scope boundary

This task changes **what is recorded**. It adds no tables, no migrations, no read
API, and no read-model. The offline graph stays a pure function of replay, per
`prd.md` R8 and the warning in `crates/hya-proto/src/projection_tree.rs`.

## 2. Target offline model (informative)

The viewer, built next, will derive a flat document. It is stated here only to
justify which fields must exist.

```
nodes[]  one per SessionId
  { session, agent, model, workdir, title, created_at, parent }

edges[]  typed
  spawn  { from: parent_session, to: child_session, member,
           tool_call, directive, description, depth, status, summary }
  mail   { from: sender_handle,  to: MailEndpoint, at: seq }
  fork   { from: source_session, to: forked_session, before_message }
```

The **tree is derived from `spawn` edges only**. `mail` and `fork` are additional
edges that a tree cannot express — this is why the target model is flat.

Cross-agent ordering needs no new field: `event_log.seq` is one global
`AUTOINCREMENT`, so all agents' events already share a total order.

## 3. C1 — `Event::ContextCompacted`

New additive variant in `crates/hya-proto/src/event.rs`, appended to the log of
the session that compacted (uniform treatment; no parent mirror).

```rust
ContextCompacted {
    /// Session whose context was compacted.
    session: SessionId,
    /// System message carrying the HYA_COMPACTED_CONTEXT marker = the output.
    message: MessageId,
    /// Which compaction path produced it.
    strategy: CompactionStrategy,
    /// First message folded into the summary.
    from_message: MessageId,
    /// Last message folded into the summary.
    to_message: MessageId,
    /// Number of messages folded.
    folded_count: u32,
    /// Estimated input tokens that tripped the threshold.
    input_tokens_est: u64,
    /// Threshold in force when it tripped.
    threshold: u64,
}
```

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// Provider-native `/responses/compact`.
    Native,
    /// Local model summarizer fallback.
    #[default]
    LocalSummarizer,
}
```

**Why pointer-only.** Nothing is ever deleted, so `from_message..=to_message`
plus the log reconstructs the exact summarizer input. Embedding the rendered text
would duplicate the whole folded transcript on every compaction.

**Why `MessageId` and not `seq`.** `seq` is assigned at append, so it is not known
when the event is constructed. `MessageId` is replay-stable and matches how
`compacted_messages()` already slices.

**Range semantics.** Must be defined per strategy and asserted in tests:

- `LocalSummarizer` — folds `messages[..len - keep_recent]`. So `from_message` is
  the first of that slice, `to_message` the last, `folded_count = len - keep_recent`.
- `Native` — `compact_if_supported` receives the whole window, so the range is the
  whole input: first and last of `messages`, `folded_count = messages.len()`.

`Event::session()` (`event.rs:545`) must return `Some(session)` for the new
variant. The projection reducer needs no state change — the variant is a record,
not a state transition. Confirm the reducer's catch-all does not reject it.

### Emit site

`crates/hya-core/src/engine/turn.rs:571-636`, inside the round loop, after the
marker message is injected and its `MessageId` is known. Both branches emit.
Use `emit_for_actor` with the turn's `actor_claim`, matching the surrounding code.

`input_tokens_est` is `estimate_tokens(&messages)` — already computed by
`needs_compaction`. Reuse it rather than recomputing; capture it once before the
branch.

## 4. C2 — Persist the local-compact output

### Current shape

`compact_with` (`crates/hya-core/src/compaction.rs:135`) both summarizes and
rebuilds the message vector. `turn.rs:619` assigns the result to a local
`messages`, so it dies with the request.

### Change

Split summarization from vector rebuilding so the turn loop can persist, mirroring
the native path exactly.

Add to `compaction.rs`:

```rust
/// What a compaction would fold, and the summary produced for it.
pub struct CompactionPlan {
    pub summary: String,
    pub from_message: MessageId,
    pub to_message: MessageId,
    pub folded_count: u32,
}

/// Summarize the foldable prefix of `messages`, or `None` if under threshold.
pub async fn plan_compaction(
    messages: &[Message],
    cfg: &CompactionConfig,
    summarizer: &dyn Summarizer,
    options: SummarizeOptions,
) -> Result<Option<CompactionPlan>, CoreError>;
```

`turn.rs` then does what the native branch already does:

1. `plan_compaction(...)` → `Some(plan)`
2. inject `format!("{COMPACT_CONTEXT_MARKER}\n{}", plan.summary)` via
   `inject_system_message_for_actor` / `inject_system_message` — the same
   claim-aware pair the native branch uses at `turn.rs:592-598`
3. emit `ContextCompacted` with the injected `MessageId` and the plan's range
4. re-read the projection and re-derive `messages`, exactly as `turn.rs:600-601`

**`compact_with` is kept and left behaviourally unchanged.** It is public API
(`lib.rs:59`) and removing it would be a breaking change outside this task's
scope. It becomes a thin wrapper over `plan_compaction`.

### Behavior change and its blast radius

After this change, a local-summarizer compaction is sticky: the next round's
`compacted_messages()` slices from the new marker instead of replaying full
history. That is the intent — it also removes the repeated summarizer call — but
it is the only model-visible change in this task.

Required regression test: run two rounds over a transcript above threshold with a
counting fake `Summarizer`; assert the summarizer is invoked **once**, not twice,
and that round two's request begins at the marker.

`Message` has no `id()` accessor today. Add a small `impl Message { pub const fn
id(&self) -> MessageId }` in `hya-proto` — needed to compute range endpoints.

## 5. C3 + C4 — `MemberSpawned` gains directive and tool call

```rust
MemberSpawned {
    // ... existing fields unchanged ...
    /// Verbatim parent directive that defines this member's purpose.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    directive: String,
    /// Tool call that caused this spawn, when it came from a tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call: Option<ToolCallId>,
}
```

Both `#[serde(default)]`, matching the existing precedent on `agent_type` and
`mode` in the same variant. Older logs deserialize with empty/`None`.

**Directive source.** `MemberSpec.directive` already holds it
(`crates/hya-core/src/subagent.rs:152`). `run_member` emits `MemberSpawned` at
`subagent.rs:239` *before* consuming `spec.directive` at `:301`, so it must clone
the directive for the event. Verbatim and unbounded, per `prd.md` R3.

**Tool call source.** `ToolOperation` (`crates/hya-tool/src/tool.rs:137`) holds
`source_tool_call_id` and exposes `source_tool_call_id()` at `:163`. It rides on
`SpawnRequest.operation` (`crates/hya-tool/src/spawn.rs:83`) and reaches the
orchestrator. `MemberSpec` does not carry it, so the plumb is:

```
SpawnRequest.operation.source_tool_call_id()
  -> MemberSpec.tool_call: Option<ToolCallId>   (new field)
  -> MemberSpawned.tool_call
```

`Option` because not every member originates from a tool call — resident members
spawned by the supervisor do not.

`MemberProjection` gains matching fields so the reducer carries them into
`build_run_tree` output. Check `projection.rs:536` and `:761`.

## 6. C5 — Fork provenance

### Finding

Forks record nothing. `session_fork.rs:33` passes `parent: None`;
`copy_messages_to_session` (`crates/hya-core/src/engine/fork.rs:10`) re-emits
every message with a **new** `MessageId`; the `before` cut point is discarded
after slicing. Only the `" (fork #N)"` title suffix survives.

### Decision: a distinct event, not `SessionCreated.parent`

Reusing `parent` is rejected. `parent` means *subagent lineage* — it feeds
`session_lineage`, subagent depth, governor budgets, and the team root used for
mailbox and roster events. Setting it on a fork would make a fork appear as a
subagent child in the spawn tree, corrupting depth accounting and the derived
tree. The tree derives from spawn edges only; a fork is a different edge type.

```rust
SessionForked {
    /// The new forked session.
    session: SessionId,
    /// Session it was forked from.
    source: SessionId,
    /// Cut point: fork copied messages strictly before this id. None = full copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before_message: Option<MessageId>,
}
```

Emitted on the **forked** session's log, immediately after creation in
`session_fork.rs`, before `copy_messages_to_session`.

Copied messages keep their new ids. The viewer reconstructs the correspondence
positionally from `source` + `before_message`; recording an id mapping is not
required by any acceptance criterion and is deliberately out of scope.

## 7. Compatibility

| Concern | Handling |
| --- | --- |
| Old logs, new binary | New fields `#[serde(default)]`; new variants simply absent |
| New logs, old binary | Folded via the existing `Event::Unknown` path |
| Migrations | None. No table changes. `prd.md` AC11 |
| Public API | `compact_with` kept and behaviourally unchanged |
| `hya-proto` weight | New variants use existing id types only; no new deps |

## 8. Risks

1. **R2 stickiness (high).** The only model-visible change. Mitigated by the
   two-round summarizer-count test and by AC9 asserting the parent's input is
   unchanged.
2. **Range semantics differ per strategy (medium).** Native folds the whole
   window, local folds a prefix. Easy to conflate. Each gets its own assertion.
3. **`MemberSpec` threading touches the spawn path (medium).** The orchestrator
   and resident supervisor both build `MemberSpec`. Both construction sites must
   set the new field; `Option` keeps the resident path honest rather than forcing
   a fake id.
4. **Directive size (low).** Unbounded by decision. Directives are already stored
   once as the child's first user message, so this roughly doubles that one
   string per spawn.
5. **Reducer coverage (low).** New variants must not fall into a rejecting arm.
   Verified by an explicit replay test.

## 9. Test plan

| AC | Test |
| --- | --- |
| AC1, AC4 | Unit: force compaction, assert one `ContextCompacted` with a range covering exactly `folded_count` messages |
| AC2 | Two tests: fake provider advertising native compact; fake summarizer for the local path |
| AC3 | **Primary regression.** Counting `Summarizer`; two rounds above threshold; assert one invocation and that round two starts at the marker |
| AC5 | Spawn a member and resume one; assert `directive` verbatim in both |
| AC6 | Assert `MemberSpawned.tool_call` matches a tool part in the parent trajectory |
| AC7 | Fork with and without `before`; assert `SessionForked` recovers source and cut point |
| AC8 | Replay a fixture log written before this task; assert unchanged projection. Replay a log with every new variant |
| AC9 | Run with spawns and a child compaction; assert parent `CompletionRequest` is byte-identical to a pre-change baseline |

Existing suites that must stay green: `crates/hya-core/tests/turn_loop.rs`,
`subagent.rs`, `fixed_system_agents.rs`, `crates/hya-server/tests/compat_session_v2_compact_api.rs`,
`compat_session_summarize_api.rs`.
