# How do Workflow loop continuations and verifiers use team capacity?

Every worker turn and judgment is a resident activation. Settlement keeps the
worker identity, installed runtime, sidecar and claim; a report releases its
claim. A continuation reactivates the same member, using fenced capacity-checked
revival only after reporting. Each verifier judgment registers a fresh parked
resident and ends it after the waiter resolves (settlement or Done report),
including failures and cancellation. Mid-run capacity errors remain typed
WORKFLOW_OVERLOADED; all run-owned actors end on an unsuccessful exit.

# Why does parked registration return before initializer hooks finish?

The Workflow controller needs the committed identity to cancel an activation
while session.start is blocked. Registration installs the episode before
returning its receipt; initialization remains owned by that episode and must
finish before its turn begins. Ordinary task registration still waits for its
initializer. No SQL connection or transaction spans a hook await.

# Who owns cancellation while a verifier judgment is pending?

`IterationDriver` passes the actual run token to `IterationGate::judge`. Each
judgment honors cancellation and finishes its own cleanup before returning; the
driver never races and drops the judgment. Workflow worker and verifier
controllers await resident endings, so RootDriverActivity covers their cleanup.
Wrap-up errors propagate, and cancelled loop reports retain the latest worker
evidence. No separate unconnected driver token or executor task is needed.

# Can Workflow actors follow the deadline notice that asks them to report?

App-composed resident members have the mandatory report coordination tool,
including narrow resource views. The loop/report regression actually calls it
from both worker and verifier. Core embedders may omit the governor, however;
turn depth then defaults to zero, so report is hidden even on a child session.
Parent presence alone therefore does not establish capability. Deadline notices
now select the report instruction only when the current request offers report;
otherwise they tell the actor to finish its turn (which settles the activation).
The regression exercises workers and verifiers both with and without a governor.
