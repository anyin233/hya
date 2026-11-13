# When is an activation settled?

Only at the episode's Idle decision, after its durable cursor covers the activation
boundary and no pending work, unread incoming mail or live direct child remains.
The route stays installed through follow-up turns. A consumed/empty trailing wake
still reaches Idle without a model request. Child endings and discarded-mail
consumption notify a waiting activation without arming another model turn.

# Where is the Workflow directive delivered and when is it pending?

Both live activation and revival use the existing parent-child DM channel; there
is no handle fallback. Guidance, route, completion and pending=true are installed
together before the turn lease is released, including a concurrent revival that
finds the member already live. This makes wait, user cancellation and quiescence
correct before bus delivery. Blank directives have no lease or mail effects.
Live/archive selection happens under claim-write serialization with endings.

# What does unsuccessful Workflow cleanup own?

Each run actor's live descendants are ended deepest-first before that actor.
Unrelated task members retain their claims. Endings remain episode-owned and
awaited under the run's RootDriverActivity.

# Does reasoning alone answer an interrupted resident wake?

No. Recovery counts only non-empty Text or Tool parts. Reasoning-only output can
be omitted by providers, so a fenced recovery supplies a visible cancelled Text
answer before the next directive. No database compatibility or schema layer is
introduced.
