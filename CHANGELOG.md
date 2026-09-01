# 0.36.7

## Workflow stage model routing

- When a Workflow Stage or loop verifier declares an explicit model assignment,
  route that activation through its ordered model fallback chain with
  per-candidate reasoning effort.
- Keep explicit Stage routes request-local so existing Agent, category, and
  provider fallback behavior remains unchanged when no assignment is present.
- Record bounded route selection and outcome metadata for replay-safe Workflow
  state. The route outcome contains no prompts, provider text, or credentials.
