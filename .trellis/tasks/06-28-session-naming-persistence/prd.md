# Session naming and persistence

## Goal

Implement durable, OpenCode-compatible session naming and persistence for hya so sessions survive crashes through the SQLite event log, are addressed internally by stable `hysec_` IDs, and are presented to users by human-readable names in switch/session lists.

## Confirmed Facts

- hya is event-sourced: `SessionEngine::emit` appends every emitted `Event` to `hya-store` SQLite `event_log`, then publishes the resulting `Envelope` to hooks and the live event bus.
- `SessionId` is currently UUID-backed and displays/parses as `ses_<uuid-simple>` in `crates/hya-proto/src/ids.rs`.
- `hya-store` already uses SQLite and has `event_log`, `session`, `message`, and `part` tables from `crates/hya-store/migrations/0001_init.sql`, but `SessionStore` currently persists/replays from `event_log` only for session state.
- Server OpenCode session list/search reads the SQLite-backed event log via `SessionStore::list_sessions` plus projection replay.
- Backend/TUI session switching currently uses `HistoryStore`, a separate JSON metadata/events store under `~/.hya/history`, and displays `last_user_message` when the title is `Untitled session`.
- Current server prompt logic auto-titles from the first prompt truncated to 50 chars; exact OpenCode summarizer parity still needs source-backed confirmation before implementation.

## Requirements

- Every newly-created hya session must receive a UID formatted as `hysec_<suffix>`.
- `<suffix>` must be 20 characters of true-random alphanumeric data.
- Session IDs used by hya APIs, switching, projection replay, and persistence must round-trip through the `hysec_` format.
- Session metadata and messages must be stored in the unified SQLite-backed session database, not in a separate JSON history store.
- Every non-streaming session/message event must be durably written as the event occurs for crash recovery.
- Streaming assistant content must still be live-visible during streaming, but the durable message content must be written only once the stream completes.
- Session title generation must match OpenCode's logic for summary naming.
- A created session with no user/assistant interaction and no generated title must be discarded on exit instead of appearing in session lists.
- An unnamed, non-empty session must receive fallback title `Untitled Session_<year>-<month>-<date>-<hour>-<minutes>` until the OpenCode-compatible naming logic replaces it.
- Switch/session selection UI must display session names/titles, never raw UIDs as the primary label.
- Existing OpenCode-compatible server session endpoints must continue to list/filter/search sessions from the same source of truth.

## Acceptance Criteria

- [ ] New `SessionId::new()` values display as `hysec_` plus exactly 20 alphanumeric characters, and `FromStr` accepts that shape.
- [ ] Session creation, prompt admission, assistant completion, title update, replay, list, and delete work using `hysec_` IDs in SQLite-backed tests.
- [ ] A no-interaction unnamed session is removed by the exit/finalization path and does not appear in switch/session lists.
- [ ] A non-empty unnamed session receives the exact fallback title format with UTC/local time behavior documented in `design.md`.
- [ ] After OpenCode-compatible naming runs, the fallback title is replaced by the generated title.
- [ ] Session switch/list surfaces display title/name text as the primary option label and keep UID only as the internal selection value.
- [ ] Streamed assistant text is live-published during streaming but durable replay after completion contains the final text without requiring every delta to have been individually committed.
- [ ] Existing OpenCode-compatible session list/search APIs still return sessions from SQLite projection and include the expected title.
- [ ] Verification includes RED→GREEN tests, full relevant Rust checks, and a real CLI/API surface exercise.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Complex task: `design.md` and `implement.md` are required before `task.py start`.

## Resolved Questions

- Exact OpenCode title-generation prompt/API/trigger semantics are resolved in `findings.md`: upstream OpenCode defaults root sessions to `New session - <ISO8601 UTC>` and child sessions to `Child session - <ISO8601 UTC>`; auto-title runs only for root sessions still on a default title with exactly one real user turn, strips `<think>...</think>`, stores the first non-empty line, and caps at 100 characters.
