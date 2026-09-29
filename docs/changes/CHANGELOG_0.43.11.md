# 0.43.11

## Context occupancy reports what the request actually carries

- The provider-measured occupancy is now anchored on the latest round's prompt (`input + cache_read + cache_write`) plus an estimate of what was appended since. Previously a turn's cumulative usage across all rounds was used as the anchor, which made the measured figure implausible and pushed `auto` accounting back to a local estimate for the whole session.
- The local estimate counts reasoning only where the route's encoder sends it: never for OpenAI Chat or Google, only the opaque provider data for Responses, and only signed current-turn thinking for Anthropic. The Anthropic encoder and the estimator share one predicate (`ReasoningReplayPolicy::replays`). In one traced session the old estimate was 1.24M tokens against an actual prompt of about 280k.
- Route-agnostic estimates (for example the summary input size) still count every reasoning part, which over-counts rather than under-counts.
