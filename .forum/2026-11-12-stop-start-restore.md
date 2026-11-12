# How are interrupted daemon turns restored after stop/start?

ADR-0031 replaces listener handoff with cancellation and a fresh daemon.
`SessionEngine::stop_for_restart` snapshots unfinished calls before cancellation.
Each cancelled turn's finish and its eligible root's restore row commit in one
transaction. Child sessions stay under the supervisor; archived roots get no
restore. The next process records prompt injection in the queue row and deletes
the row only when the restored assistant message starts. A crash between those
events starts the turn on the next launch without injecting again.
Restores use the normal server guidance, external directories, and RunGuard;
one session's failure is logged with its id and does not block the others.
It never manufactures a successful result for an unanswered permission or
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
