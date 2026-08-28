---
title: Provider Resilience Boundary
description: Retry and failover contracts for Rust model-provider requests.
---

# Provider Resilience Boundary

`hya-provider` owns model transport resilience. A completion follows this path:

```text
ProviderRouter::stream
  -> matching Provider routes in registration order
  -> HttpProvider request attempts
  -> returned EventStream
```

## Safe automatic recovery

`HttpProvider` makes at most three request attempts before response headers
establish a stream. It retries connection/transport failures, HTTP 429, and HTTP
5xx responses. The default delay is bounded exponential backoff with jitter. A
valid `Retry-After` value takes precedence and is capped at 30 seconds. Reading
a non-success body for diagnostics is capped at 2 seconds.

After one matching provider exhausts those attempts, `ProviderRouter` may try
the next registered provider that claims the same model. Routing order remains
configuration order.

## No-replay boundary

Automatic retry and provider failover stop as soon as a provider returns an
`EventStream` after successful response headers. Any later SSE error is delivered
to the caller and the request is not replayed. This is the boundary that prevents
duplicated streamed output, tool calls, and external tool side effects.

Stalls are bounded on both sides of that boundary. Each request attempt must
receive response headers within a 60-second deadline; a miss is classified as a
retryable `ProviderError::Transport`, so the three-attempt retry and router
failover apply unchanged. An established SSE stream must deliver its next frame
within a 5-minute idle deadline — the window opens at the headers (first event)
and resets on every delivered frame, so continuous streams keep unlimited
lifetime while normal reasoning think time cannot trip it. A deadline hit there
is a post-stream failure: it terminates that stream with a single error and is
never replayed or failed over.

HTTP 4xx responses other than 429, request/protocol incompatibility, JSON/decode
errors, and unknown models are not retryable. Authentication expiry is handled
by the one-shot auth recovery level above rather than generic retries.

## Auth recovery level (forced refresh)

Below the transport-retry level sits one credential-recovery level, aimed at the
single most common silent failure of long-lived OAuth sessions: an access token
that expires server-side between local expiry checks and surfaces as a
pre-stream HTTP 401 or 403.

When a route is built with `HttpProvider::with_auth_refresher`, a pre-stream
401/403 invokes that hook once with the credential material used by the failed
request, re-resolves auth headers, and retries exactly once. The hook fires
only when the resolved credential actually rotated; if the hook is absent,
fails, or yields an unchanged token, the original status error surfaces
unchanged. Bounds that hold by construction:

- At most one forced-refresh retry per request, never a loop; the retry occupies
  one of the existing `MAX_REQUEST_ATTEMPTS` slots instead of extending them.
- Strictly pre-stream: only responses that never became an event stream are
  eligible, so this level cannot replay streamed output or tool side effects.
- `ProviderError::AuthExpired` remains non-retryable for router/engine failover:
  re-login is a human action, and refresh-hook failures surface the original
  error rather than converting into automatic retries.

On the app side (`hya-app/src/oauth/ensure.rs`),
`force_refresh_access_token(_in)` bypasses the normal expiry-skew check but
keeps both guards from scheduled refresh: the single-flight process lock and a
re-read past the lock that skips the network refresh whenever storage already
moved past the exact access token that failed upstream. Concurrent streams all
rejected with 401 therefore perform exactly one network refresh and never burn
a rotated refresh token. Routes with OAuth refresh configured wire this hook at
the same `use_oauth_refresh` site that installs the bearer resolver.

Because every branch of this level ends either in a returned `EventStream`
built from a fresh response or in a pre-stream error, the no-replay boundary is
untouched: once headers establish a stream nothing is refreshed, retried, or
failed over anymore.

## Cross-model chain failover (hya-core)

`hya-provider` recovers within one model; `hya-core` recovers across models.
The session engine holds a failover plane (`SessionEngine::with_model_fallbacks`)
mapping each preferred `ModelRef` to its ordered candidate chain, populated by
the app runtime from configured `categories:` entries. Members spawned onto a
servability-picked candidate carry the forward suffix of their category chain.

When the preferred model fails before any stream exists with a retryable error
(`is_retryable_before_stream()`) or has no claiming route at all
(`UnknownModel`), the engine re-issues the identical completion request against
the next candidate and logs the switch via `tracing::warn` (from/to model).
Every attempt re-enters `ProviderRouter::stream`, so the router's preflight and
reasoning-stripping behavior applies unchanged to each candidate.
Authentication expiry and protocol incompatibility are non-retryable and fail
the turn without consuming the chain. An unset plane means exactly one direct
router call — byte-identical to the unconfigured path.

Both levels share one strict no-replay boundary: once an `EventStream` is
returned, model selection is final. A mid-stream SSE error surfaces once,
unchanged, and is never replayed or failed over onto another model.

## Error classification

`ProviderError::Transport` identifies request transport failures before stream
establishment. `ProviderError::HttpStatus` retains the numeric status, a bounded
diagnostic body snippet, and the parsed retry delay. Callers use
`is_retryable_before_stream()` rather than inspecting display strings.
