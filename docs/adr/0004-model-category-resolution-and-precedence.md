# Model category resolution and precedence

An agent may name a logical model **category** (e.g. `deep`) instead of a concrete model; the
harness resolves it to a `provider/model` at spawn time. A category is an **ordered candidate
list** and resolution picks the first candidate whose provider is configured/healthy, failing over
down the list — candidates express resilience, not a load-balancing pool. Both the agent file *and*
the main agent can decide the model, reconciled by a fixed precedence (highest wins): spawn-time
explicit `model` → spawn-time `category` override → frontmatter `model:` → frontmatter `category:`
→ global default. We recorded this because the precedence order is a real trade-off (the file names
a default, but the main agent must be able to override cost/quality at runtime) and a future reader
changing any layer would otherwise not know the intended ordering.

## Consequences

- The old `tier-cheap/strong/max/writer` placeholder builtins are removed; categories are
  config-driven concrete refs under a `categories:` block in `~/.config/hya/config.yaml`. An
  unknown category simply fails to resolve and falls through the precedence chain — no dangling
  refs.
- `hya-core::category` stays dependency-light: it takes a `Fn(&ModelRef) -> bool` "is this
  servable" predicate from the caller rather than depending on the provider router directly.
- Failover is currently spawn-time only (provider present/known); mid-stream failover on a runtime
  stream error is a preserved-but-unwired follow-up (`fallback_chain` is retained on the resolved
  category).
