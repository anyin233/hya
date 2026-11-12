# How can resident registration remain atomic without pre-commit plugin effects?

Commit the raw initial directive with the child, member, roster/channels and
claim. Install the slot and owned task before awaiting any plugin. The task
sends global/scoped `session.start`, publishes committed registration events,
persists any message-hook rewrite with `TextReplace`, notifies `agent.spawn`,
then drives the resident. Dropping the spawning caller does not drop that task.
Group-channel lookup must run inside the `BEGIN IMMEDIATE` registration
transaction; a projection read before it can split concurrent siblings.

# Why does cancelling an unstarted directive need a nonempty assistant answer?

The request mapper omits empty assistant messages. A start/finish cancellation
pair alone leaves the old user directive open in the provider request. Write a
short cancellation answer with `FinishReason::Cancelled` in the existing
stop/failure transaction alongside terminal activity and claim release.
Archive/revive's remaining transaction boundaries belong to later work.
