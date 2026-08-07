# Implementation plan — Context observability

Every step follows the `AGENTS.md` TDD gate: write one atomic failing test,
verify it fails for the expected missing behavior, implement the smallest change
that passes, then run the step's validation.

Routing per `CLAUDE.md`: steps touching `Event`/`Envelope`/wire format, the
projection reducer, or the turn/compaction lifecycle go to
`plan-executor-heavy`. Mechanical threading goes to `plan-executor-bulk`.

## Step order and rationale

S1 and S2 are pure additions with no behavior change, so they land first and de-
risk the rest. S3 is the only behavior-changing step and is isolated so it can be
reverted alone. S4–S6 are independent of S3.

---

## S1 — `Message::id()` accessor  · bulk

Add `impl Message { pub const fn id(&self) -> MessageId }` in `hya-proto`.
Needed by S2 to compute range endpoints.

- **Test:** unit assert `id()` returns the right id for all three variants.
- **Validate:** `cargo test -p hya-proto`

## S2 — `ContextCompacted` variant and strategy enum  · heavy

Wire-format change. Add `Event::ContextCompacted` and `CompactionStrategy` per
`design.md` §3. Extend `Event::session()` to return the session. Confirm the
projection reducer accepts it without a state change and does not fall into a
rejecting arm.

Does **not** emit yet — type-level only, so the diff stays reviewable.

- **Test:** round-trip serde; `session()` returns `Some`; replay a log containing
  the variant and assert the projection is unchanged and no error.
- **Validate:** `cargo test -p hya-proto`
- **Gate:** review the serde tags against neighbouring variants before S3.

## S3 — Emit `ContextCompacted` + persist local compaction  · heavy

The core step and the only behavior change. `design.md` §3 emit-site and §4.

1. Add `plan_compaction` + `CompactionPlan` to `compaction.rs`; refactor
   `compact_with` into a thin wrapper over it, behaviour unchanged.
2. In `turn.rs:571-636`, capture `estimate_tokens` once before the branch.
3. Native branch: after the existing inject, emit `ContextCompacted` with
   `strategy: Native` and the whole-window range.
4. Local branch: replace the request-local `compact_with` with
   `plan_compaction` → claim-aware inject → emit `ContextCompacted` with
   `strategy: LocalSummarizer` and the prefix range → re-read projection and
   re-derive `messages`, mirroring `turn.rs:600-601`.
5. Leave the under-threshold `else if` branch alone; `plan_compaction` returns
   `None` there.

- **Tests (AC1–AC4):**
  - one `ContextCompacted` per compaction, range covers exactly `folded_count`
  - native path emits `strategy: Native`, whole-window range
  - local path emits `strategy: LocalSummarizer`, prefix range
  - **primary regression:** counting `Summarizer`, two rounds above threshold →
    exactly one invocation, and round two's request starts at the marker
- **Validate:** `cargo test -p hya-core`, then
  `cargo test -p hya-server --test compat_session_v2_compact_api --test compat_session_summarize_api`
- **Gate:** this is the revert point. Do not proceed to S4 until green.

## S4 — `directive` + `tool_call` on `MemberSpawned`  · heavy

Wire-format change. `design.md` §5.

1. Add both fields with `#[serde(default)]` to `Event::MemberSpawned`.
2. Mirror onto `MemberProjection`; update the reducer at `projection.rs:536`
   and `:761`.
3. `run_member` (`subagent.rs:239`) clones `spec.directive` into the event —
   it is consumed later at `:301`.

- **Test (AC5):** fresh spawn and resumed spawn both carry the directive verbatim.
- **Validate:** `cargo test -p hya-proto -p hya-core`

## S5 — Thread `ToolCallId` into `MemberSpec`  · bulk

Mechanical, but escalate if any construction site is ambiguous.

Add `tool_call: Option<ToolCallId>` to `MemberSpec`. Set it from
`SpawnRequest.operation.source_tool_call_id()` at the orchestrator construction
site; set `None` at the resident-supervisor site. Populate `MemberSpawned` in
`run_member`.

- **Test (AC6):** spawned member's `tool_call` matches a tool part present in the
  parent trajectory.
- **Validate:** `cargo test -p hya-core`

## S6 — `SessionForked`  · heavy

Wire-format change. `design.md` §6. Do **not** set `SessionCreated.parent` —
that would corrupt subagent depth and the derived spawn tree.

Add the variant; emit it in `session_fork.rs` on the forked session's log after
creation and before `copy_messages_to_session`.

- **Test (AC7):** fork with and without `before`; source and cut point recover.
- **Validate:** `cargo test -p hya-proto -p hya-core -p hya-server`

## S7 — Compatibility and no-leak proof  · heavy

No production code expected; this step proves the invariants.

- **AC8:** replay a fixture log written before this task, assert the projection is
  unchanged; replay a log containing every new variant.
- **AC9:** run a scenario with spawns and a child compaction; assert the parent's
  `CompletionRequest` is byte-identical to a pre-change baseline — nothing
  recorded here leaked into any model input.
- **AC11:** assert no file was added under `crates/hya-store/migrations/`.

## S8 — Full gate

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
```

Subagent surfaces changed, so per `AGENTS.md` also:

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```

Bump `[workspace.package].version` in `Cargo.toml` and update root
`CHANGELOG.md` per the Release & Changelog Rule, moving any previous root
changelog to `docs/changes/CHANGELOG_<version>.md`.

## Commit plan

Atomic, one per step, staging only that step's files. No commit before its test
and validation pass.

| Commit | Covers |
| --- | --- |
| `feat(proto): add Message::id accessor` | S1 |
| `feat(proto): add ContextCompacted event` | S2 |
| `feat(core): emit ContextCompacted and persist local compaction` | S3 |
| `feat(proto): record directive and tool call on MemberSpawned` | S4 |
| `feat(core): thread originating tool call into member spawn` | S5 |
| `feat(proto): record fork provenance` | S6 |
| `test: prove log compatibility and context non-leak` | S7 |

## Rollback points

- After S3 — the only behavior-changing commit. Reverting it alone restores
  today's compaction behavior while keeping S1/S2 recording types in place.
- After S6 — all wire additions are in; a full revert of S2/S4/S6 restores the
  prior wire format, and old logs are unaffected either way.
