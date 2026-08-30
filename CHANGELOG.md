# 0.36.7

## Workflow stage model routing

- Route each Workflow Stage and loop verifier through an ordered model fallback
  chain with per-candidate reasoning effort.
- Keep Stage routes request-local so existing Agent, category, and provider
  fallback behavior remains unchanged when no assignment is present.
- Record bounded route selection and outcome metadata for replay-safe Workflow
  state.
