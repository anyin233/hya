# 0.36.1

## Provider and Session reliability

- OpenAI Responses and Grok streams now require a typed
  `response.completed` or `response.incomplete` terminal event. Premature EOF
  and bare `[DONE]` termination fail as decode errors; Chat Completions keeps
  its existing permissive terminal behavior.
- Compat V2 background prompts now publish durable failure and idle Events when
  a provider turn fails. Durable prompt errors are bounded to 2,048 Unicode
  scalar values. The Session releases its run reservation and accepts a later
  Turn.

## Command and Workflow behavior

- Exact `/model` and `/think` submissions now stay in the TypeScript TUI and
  open the model and reasoning-variant dialogs. Local command primaries and
  aliases suppress colliding server autocomplete rows, and exact aliases outrank
  fuzzy prefix matches.
- The Compat command catalog now publishes `/workflow` with stable metadata.
  Workflow slash execution continues through the server-owned Workflow control
  path without a parent-model request.
- Failed command requests now show a contextual TUI error toast while preserving
  prompt history and same-Session recovery.
