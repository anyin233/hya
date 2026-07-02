# Findings

## Repository facts

- `crates/hya-proto/src/ids.rs`: `SessionId` is currently UUID-backed and displays `ses_<uuid-simple>`.
- `crates/hya-core/src/engine.rs`: `SessionEngine::emit` persists the event with `SessionStore::append_event`, builds an `Envelope`, dispatches hooks, and publishes to `EventBus`.
- `crates/hya-store/src/lib.rs`: `SessionStore` persists to SQLite `event_log` and derives projections by replaying events.
- `crates/hya-store/migrations/0001_init.sql`: tables for `session`, `message`, and `part` exist but the live `SessionStore` logic does not yet materialize them for session listing.
- `crates/hya-backend/src/tui/history.rs`: backend TUI has a separate JSON `HistoryStore` with `meta.json`, `events.jsonl`, and `index.json`.
- `crates/hya-backend/src/tui.rs`: `session_summaries` maps `HistoryStore` entries to switch dialog labels and currently may show `last_user_message` instead of title for untitled sessions.
- `crates/hya-server/src/compat/session_list.rs`: Compat-compatible server session list already reads SQLite-backed sessions via `SessionStore::list_sessions` and projection snapshots.
- `crates/hya-server/src/compat/session_prompt.rs`: current `auto_title` uses first prompt truncation, not yet proven to match Compat.

## Design implications

- The event log/projection should remain the source of truth for session state to avoid SQL/reducer drift.
- If stream delta persistence is coalesced, live event publication still needs a way to deliver deltas to SSE/TUI without assigning misleading durable sequence numbers.
- Replacing `SessionId` storage from UUID bytes to `hysec_` strings likely requires store schema and parsing changes, plus compatibility thought for existing `ses_` data.

## Pending evidence

- None for planning. Exact Compat naming source was confirmed by librarian: upstream default titles are `New session - <ISO8601 UTC>` / `Child session - <ISO8601 UTC>`; auto-title runs only for root sessions still on default title with exactly one real user turn, strips `<think>...</think>`, stores first non-empty line, and caps at 100 chars.
- Parallel planner outputs were merged into `design.md` / `implement.md` with corrected plan-review failures.

## Merged planner conclusions

- Keep SQLite event log/projection authoritative; any materialized/session summary table is a rebuildable cache only.
- Use additive compatibility for old sessions: new sessions use `hysec_` IDs, legacy UUID-byte sessions remain listable/replayable/deletable.
- Centralize session ID parsing/rendering/storage-key logic; remove route-local `ses_` checks.
- Bridge TUI history to SQLite summaries instead of keeping JSON metadata as an independent source of truth.
- Split live stream publication from durable append so assistant deltas stay live while final assistant text is persisted on completion.
- Hide empty unnamed sessions from list/switch and cleanup on exit where a concrete finalization path exists.

## Empty-session finalization hook findings

- `crates/hya-core/src/engine.rs:224` already exposes `SessionEngine::delete_session(session)`, delegating to `SessionStore::delete_session`; use that deletion primitive for empty unnamed cleanup instead of inventing a parallel remover.
- `crates/hya-backend/src/tui.rs:933` begins backend TUI shutdown cleanup after the event loop exits; this is the concrete exit/finalization hook for the legacy backend TUI path.
- `crates/hya-backend/src/tui.rs:731` handles `TuiEffect::NewSession`; before replacing `session` with `new_session`, the current session should go through the same finalize-or-delete helper.
- `crates/hya-backend/src/tui.rs:772` handles `TuiEffect::ResumeSession`; before replacing `session` with `resume`, the current session should go through the same finalize-or-delete helper.
- `crates/hya-backend/src/tui/history.rs:55` eagerly creates JSON history metadata with title `Untitled session`; the SQLite finalization plan must either stop normal JSON writes or ensure JSON history is bridge/import-only so empty-session cleanup does not remain split-brain.
- Server/Compat API paths create addressable empty sessions and do not have a per-session exit event equivalent to backend TUI shutdown. Required server behavior should be list/search filtering plus explicit delete endpoint behavior; do not invent broad process-shutdown cleanup for all server sessions unless a concrete owner/session lifetime is introduced.

## Background research reconciliation

- Earlier Compat source research (`bg_0a719e5c`) found upstream does not auto-prune empty sessions; the user requirement intentionally diverges by asking hya to discard created-but-unused unnamed sessions on exit. The design must keep this divergence explicit.
- Earlier conservative planning output (`bg_f2a61b05`) recommended a core finalization helper and warned that assistant stream buffering is the main high-risk implementation area.
- Earlier failure-mode planning output (`bg_36acd66e`) emphasized explicit crash/cleanup windows and literal verification commands; the revised implementation plan should add automated cleanup tests, not only manual QA.
