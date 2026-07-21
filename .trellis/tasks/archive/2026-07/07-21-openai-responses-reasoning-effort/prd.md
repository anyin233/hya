# OpenAI Responses API and reasoning effort

## Goal

Make OpenAI-compatible providers send the selected reasoning effort correctly,
load a usable default effort for reasoning-capable models, and support the
OpenAI Responses API without removing Chat Completions compatibility.

## Background

- Current outgoing requests do not carry the configured/selected reasoning
  effort.
- OpenAI-compatible traffic currently uses the Chat Completions API.
- Reasoning-capable models need a default effort selected from configuration at
  startup.

## Requirements

- Preserve the selected reasoning effort through the complete path from config
  loading and model selection to provider request serialization.
- Every configured reasoning-capable model must have a default reasoning effort
  that is loaded when `hya` starts.
- OpenAI-compatible providers must expose `kind: openai-completion` and
  `kind: openai-response`. Existing `kind: openai` and
  `kind: openai-compatible` values remain Chat Completions aliases.
- Model config accepts the existing string entry and a detailed entry with an
  `id` plus optional `reasoning.default` and `reasoning.variants`. Detailed
  defaults are validated against the typed effort vocabulary; legacy strings
  receive the existing highest-supported fallback.
- `openai-response` must use the OpenAI Responses endpoint, request shape, SSE
  stream shape, tool calls, reasoning data, and usage reporting expected by the
  shared provider event contract.
- `openai-completion` must retain the existing Chat Completions behavior.
- Existing non-OpenAI provider behavior must remain unchanged.
- The implementation must update the workspace version and newest changelog in
  accordance with the repository release rules.

## Acceptance Criteria

- [ ] A request made with `gpt-5.6-sol` carries each reasoning effort level that
  the configured model advertises, verified at the provider request boundary.
- [ ] A provider configured for `openai-response` posts to the Responses API and
  successfully maps streamed text, reasoning, tool calls, completion, errors,
  and usage into existing events.
- [ ] A stateless Responses tool continuation replays the completed opaque
  reasoning item from the event projection before the matching function call
  and function output.
- [ ] A provider configured for `openai-completion` continues to post to Chat
  Completions and passes its existing compatibility tests.
- [ ] Config parsing rejects unsupported API option values and selects the
  configured API deterministically.
- [ ] On startup, a reasoning-capable configured model has its configured
  default effort active before the first request; the outgoing request contains
  that effort without requiring a manual model-variant switch.
- [ ] Focused tests pass, followed by `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and a local executable build.

## Constraints

- Preserve the event-sourced provider contract and avoid a parallel projection
  or provider-specific runtime channel.
- Keep `hya-proto` dependency-light and reuse existing provider protocol/model
  variant abstractions where they are sufficient.
- Do not remove or silently reinterpret existing Chat Completions configs.

## Resolved Decisions

- `kind` remains the route selector; no second provider API field is added.
- The default effort belongs to each model entry. Startup resolves the selected
  model's default once and places it on the base `AgentSpec` before the first
  request.
- A later session model switch continues to use the existing explicit variant
  path. Automatic per-model default switching is outside this task's startup
  contract.
- Responses stores only its completed opaque reasoning output item. Whole
  provider responses, upstream response IDs, and upstream function-call IDs are
  not persisted.
