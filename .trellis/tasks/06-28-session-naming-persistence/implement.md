# Session naming and persistence implementation plan

> Executor requirement: follow RED -> GREEN -> REFACTOR for every production change. Do not edit production Rust before the corresponding failing test has been run and observed failing for the expected reason.

## Overview

Implement in crate-local waves so each behavior has a failing test first and a runnable gate before the next wave. Keep the event log authoritative and avoid destructive migrations.

## Child deliverables and explicit dependencies

The work remains one Trellis task, but each deliverable below must be independently verified. Dependencies are explicit; do not rely on wave numbering alone.

| Deliverable | Verification owner | Depends on |
| --- | --- | --- |
| `hysec_` ID grammar and SQLite storage keys | `hya-proto`, `hya-store` tests | none |
| Route/client ID compatibility | `hya-server`, `hya-client` tests | `hysec_` parser/storage keys |
| Shared title policy and fallback title | `hya-core` pure tests | projection can identify session activity |
| Empty-session list filtering | server/backend list tests | shared empty-session predicate |
| Empty-session exit cleanup | core/backend/server cleanup tests | shared empty-session predicate and list filtering |
| OpenCode-compatible auto-title replacement | server prompt/summarize tests | shared title policy |
| SQLite-backed switch summaries | backend/sdk/tui tests | route/client ID compatibility and title/list behavior |
| Live-vs-durable assistant streaming | core/store tests | stable ID/store replay behavior |

## Wave 1: Session ID grammar and storage keys

### Task 1.1: Add RED tests for `hysec_` IDs

Files:

- Modify: `crates/hya-proto/src/ids.rs`

Steps:

1. Add tests that assert `SessionId::new()` / `Default::default()` displays as `hysec_` plus exactly 20 ASCII alphanumeric characters.
2. Add tests that `SessionId::from_str` accepts `hysec_ABCDEFGHIJKLMNOPQRST` and rejects wrong prefix, short suffix, long suffix, and non-alphanumeric suffix.
3. Add tests that legacy `ses_<uuid-simple>` and raw UUID still parse.
4. Add serde round-trip test for a `hysec_` session ID.
5. Run:

```sh
cargo test -p hya-proto session_id
```

Expected RED: generation test still sees `ses_<uuid>`, `hysec_` parse fails, or `storage_key` API is missing.

### Task 1.2: Implement `SessionId` representation

Files:

- Modify: `crates/hya-proto/src/ids.rs`
- Modify: `Cargo.toml` only if no existing randomness dependency can be reused from workspace dependencies.

Steps:

1. Change only `SessionId` from UUID-only to a session-specific representation that can hold `hysec_` and legacy UUID IDs.
2. Generate new IDs with OS-backed randomness and a 20-character alphanumeric suffix.
3. Preserve legacy UUID constructors/accessors only where current callers require them; prefer replacing callers with storage-key/display methods rather than spreading UUID assumptions.
4. Add `storage_key()` returning bytes for store use, or equivalent method with the same single owner.
5. Run:

```sh
cargo test -p hya-proto session_id
```

Expected GREEN: all `hya-proto` session ID tests pass.

### Task 1.3: Add RED store compatibility tests

Files:

- Modify: `crates/hya-store/tests/store.rs`
- Modify: `crates/hya-store/tests/persistence.rs`

Steps:

1. Add a store test that appends/replays/lists/deletes a new `hysec_` session.
2. Add a compatibility test that manually inserts or exercises a legacy UUID-backed session and verifies list/replay/delete still work.
3. Add a token ledger test for `hysec_` session storage key if existing token ledger tests exist in these files.
4. Run:

```sh
cargo test -p hya-store --test store session
cargo test -p hya-store --test persistence session
```

Expected RED: store still calls `as_uuid()` and cannot store/list `hysec_` sessions.

### Task 1.4: Implement store storage-key support

Files:

- Modify: `crates/hya-store/src/lib.rs`

Steps:

1. Replace all `session.as_uuid().as_bytes()` session key paths with the shared storage-key method.
2. Decode both ASCII `hysec_` keys and 16-byte UUID keys in `list_sessions`.
3. Keep `delete_session`, `replay`, `record_usage`, and `read_usage` on the same key contract.
4. Run:

```sh
cargo test -p hya-store --test store session
cargo test -p hya-store --test persistence session
```

Expected GREEN: new and legacy session storage tests pass.

## Wave 2: Route/client ID compatibility

### Task 2.1: Add RED API/client tests for `hysec_` IDs

Files:

- Modify: `crates/hya-server/tests/opencode_session_v2_create_api.rs`
- Modify: `crates/hya-server/tests/opencode_session_v2_api.rs`
- Modify: `crates/hya-server/tests/opencode_session_switch_api.rs`
- Modify: `crates/hya-client/src/lib.rs` tests if present; otherwise add the minimal test in the crate that owns URL formatting.

Steps:

1. Assert create endpoints return an ID matching `^hysec_[A-Za-z0-9]{20}$`.
2. Assert get/update/delete/prompt/switch routes accept the returned `hysec_` ID.
3. Assert legacy `ses_<uuid>` fixture routes still parse where old tests cover them.
4. Assert client URL builders use `SessionId::to_string()`, not UUID formatting.
5. Run:

```sh
cargo test -p hya-server --test opencode_session_v2_create_api
cargo test -p hya-server --test opencode_session_v2_api
cargo test -p hya-server --test opencode_session_switch_api
cargo test -p hya-client
```

Expected RED: tests or compilation fail on hard-coded `as_uuid()`/`ses_` assumptions.

### Task 2.2: Implement route/client ID changes

Files:

- Modify: `crates/hya-server/src/lib.rs`
- Modify: `crates/hya-server/src/opencode/session_v2.rs`
- Modify: `crates/hya-server/src/opencode/tui.rs`
- Modify: `crates/hya-server/src/opencode/experimental_sync.rs`
- Modify: `crates/hya-client/src/lib.rs`
- Modify other compile-error locations only where they directly assume `as_uuid()` for session URLs/validation.

Steps:

1. Route all session ID parsing through `SessionId::from_str`.
2. Remove route-local `starts_with("ses")` validation.
3. Use `session.to_string()` for URL path construction and response IDs.
4. Preserve legacy tests by updating expected IDs only when the behavior under test is new-session creation.
5. Run the commands from Task 2.1.

Expected GREEN: server/client ID compatibility tests pass.

## Wave 3: Fallback title and empty-session visibility

### Task 3.1: Add RED title-format tests

Files:

- Prefer create: `crates/hya-core/src/engine/title.rs` with tests, or use the existing module that will own title policy.
- Modify: `crates/hya-core/src/engine.rs` / `session_state.rs` only after RED.

Steps:

1. Add pure tests for `Untitled Session_%Y-%m-%d-%H-%M` with a fixed UTC timestamp.
2. Add tests for detecting hya fallback titles and upstream OpenCode default titles.
3. Add tests for title cleanup: strip `<think>...</think>`, first non-empty line, max 100 chars with `...`.
4. Run:

```sh
cargo test -p hya-core title
```

Expected RED: title helper module/functions do not exist.

### Task 3.2: Implement shared title policy

Files:

- Create/modify: `crates/hya-core/src/engine/title.rs` or a better existing module owner found during implementation.
- Modify: `crates/hya-core/src/engine/mod` declarations as needed.

Steps:

1. Implement fallback formatting using UTC timestamps.
2. Implement fallback/default-title detectors.
3. Implement title result cleanup/truncation.
4. Keep helpers pure and tested; do not call providers in this module.
5. Run:

```sh
cargo test -p hya-core title
```

Expected GREEN: title helper tests pass.

### Task 3.3: Add RED empty-session list tests

Files:

- Modify: `crates/hya-server/tests/opencode_session_v2_list_api.rs`
- Modify: `crates/hya-server/tests/opencode_session_list_api.rs`
- Modify: `crates/hya-backend/src/tui/history.rs` tests if preserving a bridge there.

Steps:

1. Create an empty session and assert list/search/switch summaries do not show it.
2. Direct get by returned ID may still work before cleanup; assert the chosen behavior from `design.md`.
3. Add a prompt/manual title and assert the session appears with title text, not raw ID.
4. Run:

```sh
cargo test -p hya-server --test opencode_session_v2_list_api empty
cargo test -p hya-server --test opencode_session_list_api empty
cargo test -p hya-backend history
```

Expected RED: empty sessions currently list, or titles use old `Untitled`/JSON behavior.

### Task 3.4: Implement empty-session filtering and fallback title assignment

Files:

- Modify: `crates/hya-proto/src/projection.rs` only if projection needs a helper for emptiness.
- Modify: `crates/hya-server/src/opencode/session_list.rs`
- Modify: `crates/hya-server/src/opencode/session_v2.rs`
- Modify: `crates/hya-server/src/opencode/projection.rs`
- Modify: `crates/hya-backend/src/tui.rs`
- Modify: `crates/hya-backend/src/tui/history.rs` only for bridge/fallback compatibility.

Steps:

1. Add one projection-based helper for “empty unnamed session” and reuse it for both list filtering and exit/finalization cleanup.
2. Apply the helper consistently in server list/search paths and TUI switch/resume summaries.
3. Use fallback title for non-empty sessions with no title.
4. Keep UID as internal selection value, not primary label.
5. Run the commands from Task 3.3.

Expected GREEN: empty-session visibility and fallback title tests pass.

### Task 3.5: Add RED finalization-cleanup tests for empty unnamed sessions

Files:

- Create/modify: `crates/hya-core/src/engine/session_cleanup.rs` or add the same logic near `SessionEngine` if no submodule split is used.
- Modify: `crates/hya-core/src/engine.rs`
- Modify: `crates/hya-backend/src/tui.rs`
- Modify: `crates/hya-server/tests/opencode_session_v2_api.rs`
- Modify: `crates/hya-server/tests/opencode_session_v2_list_api.rs`

Steps:

1. Add focused core tests for a new shared helper API, e.g. `SessionEngine::cleanup_empty_unnamed_session(session) -> Result<bool, CoreError>`, that creates an empty unnamed session, runs cleanup, then asserts the store/projection no longer contains the session.
2. Add guard tests proving the core helper does not delete a non-empty session and does not delete a manually titled session.
3. Add an API-facing cleanup proof that creates an empty session through the server test harness, verifies list output hides it, calls the core helper through the test `ServerState`/engine, then verifies direct `GET /api/session/<id>` returns not found after cleanup.
4. Add an idempotence test: repeated cleanup/delete does not fail finalization.
5. Run:

```sh
cargo test -p hya-core cleanup_empty_unnamed_session
cargo test -p hya-backend cleanup_empty_unnamed_session_on_exit
cargo test -p hya-server --test opencode_session_v2_api empty_cleanup
cargo test -p hya-server --test opencode_session_v2_list_api empty
```

Expected RED: empty sessions are only filtered from lists; no shared core helper durably deletes SQLite events, so direct lookup still succeeds after “finalization.”

### Task 3.6: Implement backend TUI finalization cleanup

Files:

- Create/modify: `crates/hya-core/src/engine/session_cleanup.rs` or add the same logic near `SessionEngine` if no submodule split is used.
- Modify: `crates/hya-core/src/engine.rs`
- Modify: `crates/hya-backend/src/tui.rs`
- Modify: `crates/hya-proto/src/projection.rs` only if Task 3.4 placed the shared “empty unnamed session” predicate there.

Steps:

1. Implement `SessionEngine::cleanup_empty_unnamed_session(session) -> Result<bool, CoreError>` as the single shared cleanup API.
2. The helper must read the current session projection, call the shared “empty unnamed session” predicate from Task 3.4, call `SessionEngine::delete_session(session)` only when that predicate is true, and return whether deletion was attempted/succeeded.
3. Wire the core helper into `crates/hya-backend/src/tui.rs::run` after pending permission/question cleanup and after the current turn is cancelled/joined.
4. Wire the same core helper before the current `session` value is replaced in `TuiEffect::NewSession` and `TuiEffect::ResumeSession`.
4. Keep list filtering as the crash-path safety net; cleanup is a best-effort durable deletion on known finalization paths, not a replacement for filtering.
5. Do not use `TerminalGuard::Drop`, panic hooks, or transport `Drop`; those paths cannot reliably await projection reads/deletes.
6. Treat cleanup as idempotent: already deleted, never persisted, or repeated finalization must not fail exit/session switching.
7. Run the commands from Task 3.5.

Expected GREEN: the shared core helper durably deletes empty unnamed sessions; backend TUI exit/new/resume call the helper; post-cleanup direct GET/store lookup returns not found; non-empty and manually titled sessions survive; repeated cleanup does not fail.

## Wave 4: OpenCode-compatible auto-title replacement

### Task 4.1: Add RED auto-title trigger tests

Files:

- Modify: `crates/hya-server/tests/opencode_session_summarize_api.rs`
- Modify: `crates/hya-server/tests/opencode_prompt_async_api.rs` or the prompt test file that already covers `session_prompt.rs`.
- Modify: `crates/hya-core` title tests if trigger eligibility is pure.

Steps:

1. Test root session with exactly one real user turn and fallback/default title gets auto-title replacement.
2. Test child session does not auto-title.
3. Test manual title does not get overwritten by later prompt.
4. Test title output cleanup/truncation on `<think>` and multi-line title.
5. Run:

```sh
cargo test -p hya-server --test opencode_session_summarize_api title
cargo test -p hya-server --test opencode_prompt_async_api title
cargo test -p hya-core title
```

Expected RED: current prompt path truncates the first prompt to 50 chars and does not use OpenCode gates.

### Task 4.2: Replace first-prompt truncation with shared title trigger

Files:

- Modify: `crates/hya-server/src/opencode/session_prompt.rs`
- Modify: `crates/hya-server/src/opencode/session_summarize.rs` if summarizer reuse is viable.
- Modify: `crates/hya-core/src/engine/summary.rs` only if existing summarizer abstraction belongs there.

Steps:

1. Remove or bypass the 50-character first-prompt truncation behavior.
2. Reuse the existing summarizer/title mechanism where possible.
3. Gate title generation exactly as described in `design.md`.
4. Write title through `SessionEngine::set_title` / `SessionTitled`.
5. Run the commands from Task 4.1.

Expected GREEN: title trigger tests pass without weakening manual rename behavior.

## Wave 5: SQLite-backed TUI/session switching

### Task 5.1: Add RED switch/resume summary tests

Files:

- Modify: `crates/hya-backend/src/tui.rs` tests if present or add focused unit tests around summary mapping.
- Modify: `crates/hya-sdk/src/store.rs` tests for title/session updates.
- Modify: `crates/hya-tui/src/app/runtime.rs` tests if test scaffolding exists.

Steps:

1. DB-only session summaries show title/fallback as primary label.
2. JSON-only legacy history can be imported/bridged once without duplicate events.
3. Mixed DB+JSON prefers DB title/timestamps.
4. Assistant stream text is never used as a fallback session title.
5. Run:

```sh
cargo test -p hya-backend session_summaries
cargo test -p hya-sdk session_updated_captures_title
cargo test -p hya-tui session
```

Expected RED: current backend TUI uses JSON `HistoryStore` and last-message fallback.

### Task 5.2: Implement SQLite-backed summary bridge

Files:

- Modify: `crates/hya-backend/src/tui.rs`
- Modify: `crates/hya-backend/src/tui/history.rs`
- Modify: `crates/hya-sdk/src/client.rs`
- Modify: `crates/hya-tui/src/app/runtime.rs`

Steps:

1. Build session summaries from SQLite store/projection where the store is available.
2. Keep JSON history as bridge/import only; make repeated import idempotent before enabling automatic import.
3. Remove assistant-text fallback from switch labels.
4. Ensure selection values still carry the real session ID.
5. Run the commands from Task 5.1.

Expected GREEN: switch/resume summary tests pass.

## Wave 6: Streaming live-vs-durable split

### Task 6.1: Add RED stream durability tests

Files:

- Modify: `crates/hya-core` tests or add a focused test module near `stream_round.rs`.
- Modify: `crates/hya-store/tests/persistence.rs` if replay behavior is easiest to assert there.

Steps:

1. Use a fake provider stream that emits multiple assistant text deltas.
2. Subscribe to the event bus and assert live deltas are observed during streaming.
3. Replay from store after completion and assert durable text equals the final assistant text.
4. Assert replay does not require every token delta to have been committed separately; if exact event counts are asserted, count final durable semantic events, not live envelopes.
5. Run:

```sh
cargo test -p hya-core stream_round
cargo test -p hya-store --test persistence stream
```

Expected RED: live and durable paths are currently coupled through `emit`.

### Task 6.2: Implement live-only publish and durable finalization

Files:

- Modify: `crates/hya-core/src/engine.rs`
- Modify: `crates/hya-core/src/engine/stream_round.rs`
- Modify: `crates/hya-proto/src/projection.rs` only if projection cannot fold final full-text delta correctly.

Steps:

1. Add live-only publish method that does not append to `SessionStore`.
2. Buffer assistant text deltas by message/part during stream collection.
3. On successful stream completion, durable-emit final assistant text sequence.
4. Preserve durable immediate writes for user prompt admission and non-streaming events.
5. Treat tool/non-text assistant events explicitly; do not drop them.
6. Run the commands from Task 6.1.

Expected GREEN: stream durability tests pass.

## Wave 7: Cleanup, full verification, and manual QA

### Task 7.1: Full Rust gates

Run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all pass, except any unrelated pre-existing failure must be captured with exact test name and reason.

### Task 7.2: Manual CLI/API QA

Use a temporary database path and drive the real surface:

```sh
tmpdir=$(mktemp -d)
HYA_DB="$tmpdir/hya.db" cargo run -p hya-backend -- sessions
HYA_DB="$tmpdir/hya.db" cargo run -p hya-backend -- exec "Say hello"
HYA_DB="$tmpdir/hya.db" cargo run -p hya-backend -- sessions
HYA_DB="$tmpdir/hya.db" cargo run -p hya-backend -- serve --bind 127.0.0.1:0
```

For the server surface, use the actual printed bind address and verify:

```sh
curl -sS http://127.0.0.1:<port>/api/session
curl -sS -X POST http://127.0.0.1:<port>/api/session
curl -sS http://127.0.0.1:<port>/api/session/<hysec_id>
```

Observed outcomes required:

- New session IDs match `hysec_[A-Za-z0-9]{20}`.
- Empty unnamed session does not appear in list/switch output after cleanup/list filtering.
- After normal backend TUI finalization cleanup, direct GET/store lookup for the captured empty unnamed session ID returns not found.
- Non-empty unnamed session appears with `Untitled Session_YYYY-MM-DD-HH-MM` before auto-title replacement.
- After title generation, the generated title replaces the fallback.
- Switch/list labels show title text as primary, not the raw UID.
- Restart with the same SQLite DB still lists and replays the non-empty session.

Additional empty-session finalization QA:

1. Start with a fresh temporary SQLite database.
2. Create a session through the real UI/API and capture the ID.
3. Do not send a prompt and do not manually title it.
4. Trigger the backend TUI finalization path.
5. Restart with the same database.
6. Verify the captured empty unnamed session is absent from list output and direct GET by ID returns not found.
7. Repeat with a non-empty session and verify it survives finalization/restart.
8. Repeat with a manually titled empty session and verify it survives finalization/restart.

### Task 7.3: Post-write review loop

For every modified Rust file, run pure LOC measurement and check the programming-skill review items:

```sh
awk '!/^[[:space:]]*$/ && !/^[[:space:]]*(\/\/|#|--)/' <changed-rust-file> | wc -l
```

If a touched source file exceeds 250 pure LOC or the review finds structural smells, stop and use the refactor skill before completion.

## Abort / rollback gates

- If `hysec_` cannot be represented without rewriting `MessageId`/`PartId`, stop and redesign; do not broaden all ID types casually.
- If store compatibility tests show legacy sessions cannot be listed/replayed, stop before server/TUI changes.
- If live stream deltas cannot be delivered without durable seq numbers, consult Oracle before changing event semantics.
- If JSON history import is not idempotent, keep it manual/disabled and do not make it part of startup.
- If any route-local ID parser remains after Wave 2, stop and remove duplication before continuing.
- If the chosen cleanup seam is a synchronous `Drop`, panic hook, terminal restore guard, or any path without both the current `SessionId` and `SessionEngine`, do not implement deletion there; keep list filtering as the safety net and consult Oracle before adding a new lifecycle endpoint.
- If cleanup can only be proven by list omission while direct GET/store lookup still succeeds after finalization, treat the cleanup wave as FAIL.
- If cleanup requires a second empty-session predicate that can drift from list filtering, stop and extract one shared predicate before wiring deletion.
- If repeated cleanup/delete can fail the exit or session-switch path, make cleanup idempotent before proceeding.

## Exact final verification command set

```sh
cargo test -p hya-proto session_id
cargo test -p hya-store --test store session
cargo test -p hya-store --test persistence session
cargo test -p hya-server --test opencode_session_v2_create_api
cargo test -p hya-server --test opencode_session_v2_api
cargo test -p hya-server --test opencode_session_switch_api
cargo test -p hya-server --test opencode_session_v2_list_api empty
cargo test -p hya-server --test opencode_session_v2_api empty_cleanup
cargo test -p hya-server --test opencode_session_list_api empty
cargo test -p hya-server --test opencode_session_summarize_api title
cargo test -p hya-server --test opencode_prompt_async_api title
cargo test -p hya-core title
cargo test -p hya-core cleanup_empty_unnamed_session
cargo test -p hya-core stream_round
cargo test -p hya-backend session_summaries
cargo test -p hya-backend cleanup_empty_unnamed_session_on_exit
cargo test -p hya-sdk session_updated_captures_title
cargo test -p hya-tui session
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
