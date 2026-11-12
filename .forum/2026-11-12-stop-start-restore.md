# How are interrupted daemon turns restored after stop/start?

ADR-0031 replaces listener handoff with cancellation and a fresh daemon.
`SessionEngine::stop_for_restart` drains active turns, then records one restore
prompt per interrupted session. The next process consumes each queue row in
the same transaction that appends its user-role prompt. It starts a fresh turn;
it never manufactures a successful result for an unanswered permission or
question. A tool may already have produced side effects before cancellation.

The database schema is the single `0001_init.sql` baseline. The restore queue
is keyed by session; it has no generation or transferred interaction replies.
Changing this baseline intentionally makes databases from earlier builds
unreadable. There is no migration or compatibility path.

# What can make process tests misread a successful restore?

Completed assistant text is durable as `TextReplace`; streaming `TextDelta`
is live-only. Restore prompts themselves contain a durable user `TextDelta`.
Use the typed interaction client for empty lists: protojson omits empty
repeated fields. A restarted daemon binds a new listener, so read its new URL
from `serve start --json` or discovery instead of reusing the old URL.

The stop/start scenarios live in the existing Track P
`p07_session_lifecycle.rs` test binary and use the offline scripted provider.
