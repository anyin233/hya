# Grok Build Provider Design

## Summary

Add `grok-build` as a native YAML provider kind by composing the existing OpenAI Responses implementation. Grok-specific request and stream policy stays behind one crate-private protocol adapter; existing provider behavior remains unchanged.

## Boundaries

- `hya-app` parses `kind: grok-build` and maps it to `hya_provider::ProviderKind::GrokBuild`.
- `hya-provider::HttpProvider` selects the Grok protocol, Bearer authentication, and `<base_url>/responses`.
- The shared Responses encoder continues to own canonical stateless input, function tools, reasoning, and opaque reasoning replay.
- A crate-private Grok protocol adapter adds `include: ["reasoning.encrypted_content"]` and selects strict terminal validation.
- The shared Responses decoder accepts both documented reasoning delta event names.

## Configuration Contract

`grok-build` is a native YAML `kind`; it is not an alias for a new config format. Its provider fallback reasoning variants are `low`, `medium`, and `high`. Existing model-level YAML reasoning metadata can still override provider fallbacks through the current configuration path.

No route-wide context-window or backend-search metadata is added. The supplied values are endpoint claims and the project has no matching provider configuration contract.

## Request Contract

Grok Build requests use the existing Responses body and add:

```json
{
  "include": ["reasoning.encrypted_content"]
}
```

The existing body already supplies `stream: true`, `store: false`, nested reasoning effort, ordered stateless input, flat function tools, and prior opaque reasoning items. No Grok proxy tracking headers, Chat fallback, retry policy, or hosted-search abstraction is introduced.

The additional `include` field is Grok-only to avoid changing requests sent by existing `openai-response` providers.

## Stream Contract

Both `response.reasoning_summary_text.delta` and `response.reasoning_text.delta` feed the existing normalized reasoning-delta path. This is an additive Responses decoder improvement shared by all Responses providers.

Grok Build streams succeed only after `response.completed` or `response.incomplete`. Bare `[DONE]` and EOF are transport termination, not semantic completion. Existing `openai-response` streams retain their current permissive terminal behavior.

Function calls, usage, text, opaque reasoning replay, and provider failures continue through existing normalized events. Hosted web-search events and route metadata are deferred because they were not observed on the supplied endpoint and are not needed to introduce the provider kind.

## Test Boundary

1. A native YAML config test proves `kind: grok-build` parses with the documented fallback reasoning variants and default.
2. The existing local HTTP/SSE integration harness proves endpoint, fake Bearer auth, request body, all three efforts, both reasoning delta forms, typed completion, and strict rejection of truncated streams.
3. Existing OpenAI Responses tests prove its body and terminal semantics do not regress.

Live credentials never enter test fixtures, logs, or task artifacts.

## Compatibility

- Adding the public `ProviderKind::GrokBuild` variant can affect downstream exhaustive matches; it is the required native routing surface.
- Existing YAML kinds, HTTP transport, persistence, projections, and public Responses protocol construction remain unchanged.
- No dependency or database migration is required.

## Resolved Alternatives

- **Shared encoder change versus Grok adapter:** use the adapter because encrypted-content inclusion is required for Grok Build but not required to change existing providers.
- **Global strict termination versus Grok-only strictness:** keep strictness Grok-only because existing Responses fixtures intentionally accept bare `[DONE]`.
- **Decoder-first versus config-first TDD:** start at the public YAML seam, then cover wire behavior in focused iterations.
- **Provider rewrite or metadata importer:** reject both; the existing protocol boundary already supports the required extension.

## Rollback

Remove the YAML/provider variants and Grok protocol adapter, then revert only the additive reasoning event case and task release metadata. No persisted data requires migration or recovery.

## External Validation

Retry bounded `low`, `medium`, and `high` requests against the supplied endpoint after deterministic checks. Record only effort, HTTP status, and terminal event/error class. Persistent HTTP 503 remains an external acceptance blocker and must not trigger speculative protocol changes.
