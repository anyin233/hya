# Research: Grok API and Grok Build protocol

- **Query**: Determine the Grok API and Grok Build request/streaming contracts, including endpoints, authentication, request fields, Responses versus Chat formats, SSE events, tool and search behavior, reasoning configuration, and supported Grok 4.5/Grok Build reasoning efforts.
- **Scope**: mixed (project contracts, official xAI documentation, and pinned first-party Grok Build source)
- **Date**: 2026-07-21

## Findings

### Evidence classes

| Label | Meaning |
|---|---|
| **Public contract** | Current xAI developer documentation for `api.x.ai`. |
| **Pinned implementation** | `xai-org/grok-build` commit `3af4d5d39897855bdcc74f23e690024a5dc05573` and the exact `async-openai` fork revision pinned by it. This is first-party client behavior, not a promise that every compatible endpoint emits every accepted field or event. |
| **Project contract** | Existing hya Trellis specification and task PRD. |
| **Endpoint claim** | Sanitized metadata supplied in the task; not independently observed during this research. |

### Executive result

The narrow compatibility target is OpenAI Responses format over `POST <base_url>/responses`. For public xAI, `<base_url>` is `https://api.x.ai/v1`; authentication is `Authorization: Bearer <key>`. A streaming request sets `stream: true` and receives SSE. Public xAI curl examples omit `Accept`, so `Accept: text/event-stream` is not a documented request requirement. Parsing typed SSE data and requiring a typed terminal response event are pinned Grok Build client behavior, not an explicit public xAI completion contract. [Public REST reference](https://docs.x.ai/developers/rest-api-reference/inference/chat) [Grok Build client, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/client.rs#L703-L1200)

For `grok-4.5`, the only model-supported reasoning efforts currently documented by xAI are `low`, `medium`, and `high`; `high` is the default and reasoning cannot be disabled. Grok Build's default model catalog independently exposes the same three choices. Its shared type can encode seven generic values (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`), but that type is a cross-model representation and is not evidence that Grok 4.5 accepts all seven. [Reasoning guide](https://docs.x.ai/developers/model-capabilities/text/reasoning) [Grok Build model catalog, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-models/default_models.json#L1-L38) [Generic effort type, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampling-types/src/types.rs#L765-L810)

The supplied route is described in the task as model `grok-4.5`, a Responses-style base URL, a 1,000,000-token context window, and backend search support (`prd.md:9-11`). Current public xAI and Grok Build model metadata both say 500,000 tokens. The route's larger value must remain endpoint-specific metadata unless a sanitized live response header confirms it. [Grok 4.5 model page](https://docs.x.ai/developers/models/grok-4.5)

### Sanitized endpoint observations

On 2026-07-21, `GET /v1/models` returned HTTP 200 and listed `grok-4.5` with owner `xai`. A Grok Build-shaped Responses request using `low`, a minimal Responses control request, and a Chat Completions control request each returned HTTP 503 with a generic `api_error` service-unavailable response. No successful inference stream, typed terminal event, context header, backend-search behavior, or accepted effort value was observed. The credential and response payloads were not recorded.

On 2026-07-22, the exact implemented Responses request was retried separately
with `low`, `medium`, and `high`. All three returned HTTP 503 with error type
`api_error`, no typed terminal event, and no output-text delta. Request and
response bodies and the credential were not recorded.

A follow-up differential probe on 2026-07-22 confirmed that `GET /v1/models`
still returned HTTP 200 and listed `grok-4.5`. Minimal streaming Responses
requests returned HTTP 503 both without an `Accept` header and with
`Accept: text/event-stream`. Non-streaming Responses requests returned the same
503 for both `grok-4.5` and a deliberately invalid model name. The failure is
therefore upstream of model validation and is not explained by SSE content
negotiation or Grok-specific request fields.

### Official xAI contract re-check (2026-07-22)

#### Corrections to the earlier research

1. Public xAI documentation does not require an `Accept: text/event-stream`
   request header. Its streaming curl examples send `Content-Type` and Bearer
   authorization, set `stream: true`, and omit `Accept`. Grok Build's pinned
   client header is implementation behavior, not a public API requirement.
2. xAI now explicitly calls Chat Completions a legacy/deprecated endpoint and
   says new features go to Responses. This does not make Chat invalid: current
   examples still use `POST /v1/chat/completions` with `grok-4.5`.
3. xAI publishes no HTTP 503 meaning or retry contract in its Debugging, Rate
   Limits, or inference REST reference pages. The earlier `Retry-After` and
   `x-should-retry` observations are pinned Grok Build proxy/client behavior
   only. Public retry guidance is limited to HTTP 429, for which xAI recommends
   exponential backoff.
4. Current xAI pages conflict on Grok 4.5 reasoning details. The model-specific
   Reasoning page says `low`, `medium`, and `high` (default) and demonstrates
   Responses `reasoning: {"effort": ...}`. The generated REST schema still
   describes its reasoning fields as only supported by `grok-4.3`, and the
   comparison page says legacy Chat does not return reasoning content even
   though the REST Chat response schema still contains `reasoning_content`.
   The model-specific page is the direct capability contract; the conflict does
   not explain a minimal request with no reasoning field returning 503.

#### Current public request and availability contract

| Concern | Responses | Chat Completions |
|---|---|---|
| Endpoint | `POST https://api.x.ai/v1/responses` | `POST https://api.x.ai/v1/chat/completions` |
| Minimum prompt field | Required `input` string or item array | Required `messages` array |
| Grok 4.5 reasoning request | Nested `reasoning.effort`: `low`, `medium`, or `high`; default `high` | REST schema exposes top-level `reasoning_effort`, but its Grok 4.5 annotation is stale/contradictory |
| Streaming selector | `stream: true` | `stream: true` |
| Documented stream | Data-only SSE ending in `data: [DONE]`; reasoning examples handle `response.reasoning_text.delta` and `response.reasoning_summary_text.delta` | `chat.completion.chunk` data events ending in `data: [DONE]` |
| State | Stored for 30 days by default; `store: false` opts out | Stateless; resend history |

Public examples require `Content-Type: application/json` and
`Authorization: Bearer <XAI_API_KEY>`. The REST overview describes the route
base as `https://api.x.ai`; OpenAI-compatible SDK examples configure
`https://api.x.ai/v1`, yielding the same `/v1/...` endpoints. Grok 4.5's exact
published ID is `grok-4.5`; its aliases are `grok-4.5-latest` and
`grok-build-latest`. Its model page lists 500,000 tokens and availability in
`us-east-1` and `us-west-2`.

The custom compatible base URL is not `api.x.ai`. These public contracts prove
what xAI accepts, but do not prove that the custom gateway implements the same
validation order, model entitlement, regional routing, headers, status mapping,
or retry behavior. `GET /v1/models = 200` proves only that the custom catalog
surface is reachable and advertises `grok-4.5`; it does not prove that its
inference dispatch plane is provisioned or healthy.

#### Documented errors, 503, and retry semantics

xAI's Debugging page documents request/auth failures as 400, 401, 403, 404,
405, 415, or 422, and inference rate limiting as 429. Its Rate Limits page says
exceeding RPS or TPM returns 429 and recommends exponential backoff. Neither
page lists a 5xx status, HTTP 503, `Retry-After`, or a 503 retry rule. Therefore
there is no first-party public basis to interpret this 503 as an invalid model,
invalid reasoning effort, malformed Responses body, missing SSE `Accept`
header, or rate limit. There is also no public xAI guarantee that retrying a 503
is safe or will succeed.

The official status RSS feed fetched on 2026-07-22 had a `lastBuildDate` of
2026-07-07 and its newest incidents were resolved. It records a resolved
2026-07-02 `Grok Build 0.1 unavailable` incident in `us-east-1`, attributed to
networking issues, but no feed entry establishes the health of Grok 4.5 or the
custom gateway during the 2026-07-21/22 probes. The status homepage itself
returned HTTP 403 to this research fetcher; the RSS feed was accessible.

#### Can a documented request mismatch explain the observations?

No documented xAI request mismatch accounts for the full pattern:

- xAI's own minimal Responses body needs only `model` and `input`; the custom
  endpoint returned 503 for an equivalent minimal body as well as the exact
  Grok Build-shaped body.
- `low`, `medium`, and `high` are exactly the documented Grok 4.5 efforts; all
  returned the same 503, and a body with no reasoning field also returned 503.
- Official streaming examples omit `Accept`; the custom endpoint returned the
  same 503 with `Accept` absent or set to `text/event-stream`, and with streaming
  disabled.
- Both the preferred Responses endpoint and the still-supported legacy Chat
  endpoint returned 503, so choosing one public wire contract over the other
  does not distinguish the failure.
- A valid and deliberately invalid model returned the same 503. Either the
  request fails before model validation or the custom gateway flattens distinct
  downstream failures into one generic status.

An undocumented custom-proxy requirement remains logically possible, but no
official xAI page supplies evidence for one.

#### Ranked evidence-backed causes

1. **Custom inference dispatch or routing failure (strongest).**
   The catalog route works while every inference POST variant fails uniformly,
   including requests that should diverge at model/body validation.
2. **Custom route provisioning, entitlement, or error translation failure.**
   The credential may reach the catalog while inference dispatch is unavailable
   or denied, with the gateway mapping the internal result to a generic 503.
   Public `api.x.ai` would normally use 401/403/4xx, but that mapping is not a
   contract for the custom host.
3. **Persistent upstream capacity or network incident.** Official xAI status
   history proves model/region networking outages occur, but two days of the
   same custom-route result and no matching status evidence leave this
   unconfirmed.
4. **Undocumented request mismatch (weakest).** The minimal/exact,
   Responses/Chat, stream/non-stream, header, effort, and model controls remove
   every mismatch documented by xAI; only custom behavior could remain.

#### Smallest decisive evidence

Another request-body matrix is not discriminating. Make one minimal,
non-streaming Responses request matching xAI's documented curl shape and retain
only its HTTP status, timestamp, response headers, and any gateway correlation
ID (never the credential or body). The custom endpoint operator must trace that
ID or timestamp and report whether JSON/model validation ran, which
backend/region was selected, and the upstream status/error class. `Retry-After`
or a gateway retry header, if present, is useful route-specific evidence, but
xAI's public docs do not promise either. This single server trace distinguishes
validation, authorization/entitlement, routing, and upstream availability;
additional blind retries do not.

That trace was captured at `2026-07-22T08:14:34Z`: HTTP/2 503, `server:
cloudflare`, `x-request-id: 2958b251-7626-43e9-816e-718eaaf8c9c3`, and
`cf-ray: a1f0fe1c4f69c4fe-LHR`. The response exposed neither `Retry-After` nor
`x-should-retry`. The Cloudflare location identifies the edge point, not the
xAI model region; the custom gateway operator must use the timestamp and
request ID to identify the origin dispatch failure.

#### Exact first-party sources consulted

- [Generate Text](https://docs.x.ai/developers/model-capabilities/text/generate-text)
- [Responses versus Chat comparison](https://docs.x.ai/developers/model-capabilities/text/comparison)
- [Legacy Chat Completions](https://docs.x.ai/developers/model-capabilities/legacy/chat-completions)
- [Reasoning](https://docs.x.ai/developers/model-capabilities/text/reasoning)
- [Streaming](https://docs.x.ai/developers/model-capabilities/text/streaming)
- [Grok 4.5 model page](https://docs.x.ai/developers/models/grok-4.5)
- [Models](https://docs.x.ai/developers/models)
- [Quickstart](https://docs.x.ai/developers/quickstart)
- [Inference REST API overview](https://docs.x.ai/developers/rest-api-reference)
- [Chat and Responses REST reference](https://docs.x.ai/developers/rest-api-reference/inference/chat)
- [Debugging Errors](https://docs.x.ai/developers/debugging)
- [Rate Limits](https://docs.x.ai/developers/rate-limits)
- [xAI status RSS](https://status.x.ai/feed.xml)
- [xAI status homepage](https://status.x.ai) - linked by Debugging; HTTP 403 to
  this fetcher, so no homepage state was used as evidence.

### Files Found

| File Path | Description |
|---|---|
| `.trellis/tasks/07-21-grok-build-provider/prd.md:9` | Supplied route claims and credential constraint. |
| `.trellis/spec/backend/quality-guidelines.md:391` | Existing hya protocol-selection, reasoning replay, error, and test contracts. |
| `crates/codegen/xai-grok-sampler/src/client.rs` (upstream) | Endpoint assembly, authentication, headers, request defaults, SSE decoding, response metadata, and errors. |
| `crates/codegen/xai-grok-sampler/src/stream/responses.rs` (upstream) | Normalization of typed Responses events into Grok Build sampling events. |
| `crates/codegen/xai-grok-sampling-types/src/conversation.rs` (upstream) | Conversion from conversation items to Responses input, tools, reasoning, and structured output. |
| `crates/codegen/xai-grok-sampling-types/src/types.rs` (upstream) | API backend selection and the generic seven-value reasoning-effort enum. |
| `crates/codegen/xai-grok-models/default_models.json` (upstream) | Grok Build's selected Grok 4.5 backend, context, default effort, and effort menu. |
| `crates/codegen/xai-grok-shell/src/agent/config.rs` (upstream) | Public and Grok Build proxy base-URL defaults. |
| `crates/codegen/xai-grok-tools/src/implementations/web_search/client.rs` (upstream) | Grok Build's separate client-side `web_search` function implementation. |
| `async-openai/src/types/responses/stream.rs` (pinned fork) | Exact serde `type` tags and payload structures for all 49 accepted Responses SSE events. |

### Endpoints and authentication

#### Public xAI contract

| Operation | Method and path | Notes |
|---|---|---|
| Responses create/stream | `POST https://api.x.ai/v1/responses` | Preferred shape for stateful IDs, flat tools, hosted tools, and typed events. |
| Chat create/stream | `POST https://api.x.ai/v1/chat/completions` | OpenAI Chat-compatible messages/chunks. |
| Compact Responses context | `POST https://api.x.ai/v1/responses/compact` | Produces canonical compacted input items; not required by this compatibility task. |
| Retrieve/delete stored response | `GET` / `DELETE https://api.x.ai/v1/responses/{response_id}` | Responses are stored for 30 days by default unless `store: false`. |

Headers shown by public xAI examples are:

```http
Content-Type: application/json
Authorization: Bearer <XAI_API_KEY>
```

Streaming is selected by `stream: true`. Public curl examples omit an `Accept`
request header; the response is documented as SSE.

The credential remains an environment/runtime value. No credential or prefix is present in this report. [REST examples](https://docs.x.ai/developers/rest-api-reference/inference/chat)

#### Grok Build implementation

Grok Build joins a configured base URL and one of `responses`, `chat/completions`, or `messages`; therefore a configured OpenAI-style base already ending in `/v1` becomes `<base>/responses`, with no extra `/v1` inserted. Its generic sampler can use either `Authorization: Bearer ...` or `x-api-key: ...`, selected by `AuthScheme`; public xAI examples use Bearer. [Client endpoint and auth, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/client.rs#L360-L725) [Sampler config, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/config.rs)

First-party source also contains a product proxy base, `https://cli-chat-proxy.grok.com/v1`. Requests to that product service can carry these Grok Build tracking/feature headers:

- Per request: `x-grok-conv-id`, `x-grok-req-id`, `x-grok-model-override`, `x-grok-session-id`, `x-grok-agent-id`, and optional `x-grok-turn-idx`, `x-grok-deployment-id`, `x-grok-user-id`.
- Client defaults when configured: `x-grok-client-version`, `x-grok-client-identifier`, `x-grok-deployment-id`, `x-grok-user-id`, and `User-Agent`.
- Optional proxy feature: `x-grok-doom-loop-check: true`.
- Parsed response metadata: `x-grok-context-window`, `x-grok-max-completion-tokens`, `x-models-etag`, `Retry-After`, and `x-should-retry`.

These are pinned product-proxy behavior, not required public `api.x.ai` headers. A compatible provider should not invent them for an endpoint unless endpoint evidence requires them. [Grok request headers, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/client.rs#L38-L77) [Response metadata, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/client.rs#L220-L290)

### Documented request fields

The current combined REST reference documents these request-body fields. "Unsupported" and compatibility-only annotations below are xAI's, not inferences.

#### Responses: `POST /v1/responses`

| Field | Contract summary |
|---|---|
| `input` | Required string or ordered input-item array. |
| `model` | Model ID. |
| `instructions` | System/developer instruction alternative; cannot be combined with `previous_response_id` according to the REST reference. |
| `previous_response_id` | Continue a stored response. |
| `store` | Store input/response; public default is true and retention is 30 days. |
| `stream` | Emit SSE when true. |
| `include` | Additional output, including `reasoning.encrypted_content`; output-logprob compatibility value is accepted but ignored. |
| `reasoning` | Object with `effort`, `summary`, and compatibility-only `generate_summary`. |
| `reasoning_effort` | Non-standard top-level convenience field, consulted only when `reasoning` is absent. |
| `tools` | Up to 128 function or web-search tools according to the REST reference. |
| `tool_choice` | Automatic, none, required, or selected tool behavior. |
| `parallel_tool_calls` | Permit parallel calls. |
| `max_turns` | Server-side agentic tool-turn cap; ignored for non-agentic requests. |
| `search_parameters` | Realtime search mode, sources, dates, citation flag, and result cap. |
| `text` | Output format, including JSON-schema structured output. |
| `max_output_tokens` | Output plus reasoning token cap; public default is 128,000. |
| `temperature`, `top_p`, `top_k`, `min_p` | Sampling controls. |
| `logprobs`, `top_logprobs` | Token log probabilities; ignored by `grok-4.20` and newer. |
| `prompt_cache_key` | Sticky routing/cache key, plumbed to `x-grok-conv-id`. |
| `context_management` | Parsed context directives such as compaction; documented as not yet executed. |
| `service_tier` | `default` or `priority`. |
| `user` | End-user abuse-monitoring identifier. |
| `background` | Unsupported. |
| `metadata`, `truncation` | Compatibility-only / unsupported. |

Source: [xAI REST reference, Responses request body](https://docs.x.ai/developers/rest-api-reference/inference/chat).

#### Chat: `POST /v1/chat/completions`

The documented fields are `deferred`, `frequency_penalty`, `logit_bias`, `logprobs`, `max_completion_tokens`, deprecated `max_tokens`, `messages`, `model`, `n`, `parallel_tool_calls`, `presence_penalty`, `prompt_cache_key`, `reasoning_effort`, `response_format`, `search_parameters`, `seed`, `service_tier`, `stop`, `stream`, `stream_options`, `temperature`, `tool_choice`, `tools`, `top_logprobs`, `top_p`, `user`, and `web_search_options`. The REST reference marks `logit_bias` unsupported, several controls unsupported by reasoning models, and `web_search_options` as OpenAI compatibility mapping. [xAI REST reference, Chat request body](https://docs.x.ai/developers/rest-api-reference/inference/chat)

### Responses versus Chat wire format

| Concern | Responses | Chat Completions |
|---|---|---|
| Prompt | `instructions` plus ordered `input` strings/items | Ordered `messages` with `role` and `content` |
| Function definition | Flat: `{ "type": "function", "name": ..., "description": ..., "parameters": <JSON Schema> }` | Nested OpenAI Chat shape: `{ "type": "function", "function": { ... } }` |
| Model function call | An `output` item with `type: "function_call"`, `call_id`, `name`, and JSON-string `arguments` | `choices[].message.tool_calls[]`, with nested `function.name` and `function.arguments` |
| Tool result | Input item `{ "type": "function_call_output", "call_id": ..., "output": ... }` | Message `{ "role": "tool", "tool_call_id": ..., "content": ... }` |
| Reasoning request | `reasoning: { "effort": ..., "summary": ... }` | Top-level `reasoning_effort` |
| Reasoning output | Reasoning output items and typed reasoning SSE events; encrypted content can be requested for replay | Public docs conflict: the comparison guide says none is returned, while the generated REST schema still lists `reasoning_content` |
| Token cap | `max_output_tokens` includes visible output and reasoning | `max_completion_tokens` applies to visible output; reasoning/function-call tokens are excluded by current docs |
| Structured output | `text.format` | `response_format` |
| Streaming data | JSON SSE objects tagged by `type`, then `[DONE]` | `chat.completion.chunk` deltas, then `[DONE]` |
| Continuation | `previous_response_id`, or stateless replay of prior items | Replay prior messages |
| Persistence | Stored for 30 days by default; opt out with `store: false` | No Responses-style response resource described |
| Hosted tools | Flat native tools such as `{ "type": "web_search" }` | Search is exposed through Chat-specific search fields/SDK behavior |

Public function-call documentation warns that a streamed function call is returned whole in one chunk. Grok Build's proxy can be asked for argument deltas with non-standard `stream_tool_calls: true`; that extension must not be assumed for public xAI or another compatible endpoint. [Function calling](https://docs.x.ai/developers/tools/function-calling) [Grok Build request injection, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/client.rs#L1147-L1225)

### Grok Build Responses request shape

Pinned Grok Build source converts its conversation request into a Responses request with:

- ordered `input` items, preserving messages, function calls, function-call outputs, backend-tool items, and opaque reasoning needed by later tool rounds;
- `instructions` for the system prompt;
- flat function/hosted `tools` and `tool_choice`;
- `text.format` for a requested JSON Schema;
- `max_output_tokens`, `prompt_cache_key`, `temperature`, and `top_p` when present;
- `reasoning: { effort: <selected>, summary: "concise" }` when reasoning is configured;
- `store: false` by default for zero-data-retention behavior;
- `include: ["reasoning.encrypted_content"]` so stateless continuation can replay opaque reasoning;
- `stream: true` on the streaming path;
- optional proxy-only `stream_tool_calls: true` and raw xAI-specific hosted tools such as `x_search`.

Sources: [conversation conversion, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampling-types/src/conversation.rs#L2171-L2510), [defaults and streaming, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/client.rs#L990-L1225).

A minimal public-contract request matching that behavior is:

```json
{
  "model": "grok-4.5",
  "input": [{ "role": "user", "content": "..." }],
  "reasoning": { "effort": "high" },
  "include": ["reasoning.encrypted_content"],
  "store": false,
  "stream": true
}
```

`summary: "concise"` is omitted from this minimal shape because current xAI REST documentation calls summary selection compatibility-only and says the returned summary is always detailed. It remains part of the pinned Grok Build request behavior.

### SSE transport and terminal behavior

Public xAI documents SSE enabled by `stream: true`, recommends a longer timeout for reasoning models, and shows `data: [DONE]` termination. It explicitly demonstrates `response.reasoning_text.delta` and `response.reasoning_summary_text.delta`, but does not publish a complete Responses event catalog on the streaming page. [Streaming guide](https://docs.x.ai/developers/model-capabilities/text/streaming) [Reasoning guide](https://docs.x.ai/developers/model-capabilities/text/reasoning)

Pinned Grok Build behavior is more precise:

1. Parse the byte stream as SSE.
2. Ignore/consume recognized auxiliary check events before typed decoding.
3. Stop transport on a `data` value equal to `[DONE]`.
4. Parse every other `data` value as a `ResponseStreamEvent`, discriminated by its JSON `type` field.
5. Treat non-2xx HTTP status as an HTTP/auth error before reading SSE; parse `Retry-After`, `x-should-retry`, and model metadata headers when available.
6. Normalize `response.completed` as success, `response.incomplete` as a length/incomplete terminal result, and both `response.failed` and top-level `error` as provider failures.
7. Fail a stream that ends without `response.completed` or `response.incomplete`, even if `[DONE]` closed the transport.

The pinned dependency is `async-openai` fork commit `95b52ebdedf42143083cf3d6f0e0be7c84e9c808`; its serde enum accepts the following exact 49 JSON `type` values. Payload keys below omit the common `sequence_number` for brevity. "Consumed" describes pinned Grok Build normalization, not what an endpoint is required to emit. [Exact event enum, pinned](https://github.com/our-forks/async-openai/blob/95b52ebdedf42143083cf3d6f0e0be7c84e9c808/async-openai/src/types/responses/stream.rs#L1-L160) [Normalizer, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampler/src/stream/responses.rs#L1-L390)

| # | Exact `type` | Principal payload | Pinned Grok Build handling |
|---:|---|---|---|
| 1 | `response.queued` | `response` | Accepted; lifecycle only. |
| 2 | `response.created` | `response` | Accepted; lifecycle only. |
| 3 | `response.in_progress` | `response` | Accepted; lifecycle only. |
| 4 | `response.completed` | `response` | Success terminal; final output and usage are normalized. |
| 5 | `response.incomplete` | `response` | Incomplete/length terminal; final output and usage are normalized. |
| 6 | `response.failed` | `response` | Provider failure. |
| 7 | `response.output_item.added` | `output_index`, `item` | Starts client function calls when `item.type == function_call`. |
| 8 | `response.output_item.done` | `output_index`, `item` | Completes hosted `web_search`/custom-tool output; other items remain in final response. |
| 9 | `response.content_part.added` | `item_id`, `output_index`, `content_index`, `part` | Accepted; no incremental normalized event. |
| 10 | `response.content_part.done` | `item_id`, `output_index`, `content_index`, `part` | Accepted; no incremental normalized event. |
| 11 | `response.output_text.delta` | item/output/content indices, `delta`, optional `logprobs` | Emits normalized text delta. |
| 12 | `response.output_text.done` | item/output/content indices, `text`, optional `logprobs` | Accepted; final text comes from terminal response. |
| 13 | `response.output_text.annotation.added` | indices, `annotation_index`, `annotation` | Accepted; annotation remains available in final output. |
| 14 | `response.refusal.delta` | item/output/content indices, `delta` | Accepted and counted as output; not emitted as a text delta. |
| 15 | `response.refusal.done` | item/output/content indices, `refusal` | Accepted and counted as output. |
| 16 | `response.function_call_arguments.delta` | `item_id`, `output_index`, `delta` | Emits argument delta after the matching output-item start. |
| 17 | `response.function_call_arguments.done` | `item_id`, `output_index`, `arguments`, optional `name` | Accepted; final call remains in terminal output. |
| 18 | `response.file_search_call.in_progress` | `output_index`, `item_id` | Accepted as hosted-tool progress. |
| 19 | `response.file_search_call.searching` | `output_index`, `item_id` | Accepted as hosted-tool progress. |
| 20 | `response.file_search_call.completed` | `output_index`, `item_id` | Accepted as hosted-tool progress. |
| 21 | `response.web_search_call.in_progress` | `output_index`, `item_id` | Emits hosted `web_search` started. |
| 22 | `response.web_search_call.searching` | `output_index`, `item_id` | Accepted; no separate normalized event. |
| 23 | `response.web_search_call.completed` | `output_index`, `item_id` | Accepted; full result is taken from `output_item.done`. |
| 24 | `response.reasoning_summary_part.added` | `item_id`, `output_index`, `summary_index`, `part` | Accepted; no incremental normalized event. |
| 25 | `response.reasoning_summary_part.done` | `item_id`, `output_index`, `summary_index`, `part` | Accepted; no incremental normalized event. |
| 26 | `response.reasoning_summary_text.delta` | item/output/summary indices, `delta` | Emits normalized reasoning delta. |
| 27 | `response.reasoning_summary_text.done` | item/output/summary indices, `text` | Accepted; final reasoning comes from terminal response. |
| 28 | `response.reasoning_text.delta` | item/output/content indices, `delta` | Emits normalized reasoning delta and accumulates fallback reasoning. |
| 29 | `response.reasoning_text.done` | item/output/content indices, `text` | Accepted; final reasoning comes from terminal response. |
| 30 | `response.image_generation_call.completed` | `output_index`, `item_id` | Accepted; not normalized by this text sampler. |
| 31 | `response.image_generation_call.generating` | `output_index`, `item_id` | Accepted; not normalized by this text sampler. |
| 32 | `response.image_generation_call.in_progress` | `output_index`, `item_id` | Accepted; not normalized by this text sampler. |
| 33 | `response.image_generation_call.partial_image` | `output_index`, `item_id`, partial-image index/data | Accepted; not normalized by this text sampler. |
| 34 | `response.mcp_call_arguments.delta` | `output_index`, `item_id`, `delta` | Accepted; not incrementally normalized. |
| 35 | `response.mcp_call_arguments.done` | `output_index`, `item_id`, `arguments` | Accepted; final item remains in terminal output. |
| 36 | `response.mcp_call.completed` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 37 | `response.mcp_call.failed` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 38 | `response.mcp_call.in_progress` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 39 | `response.mcp_list_tools.completed` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 40 | `response.mcp_list_tools.failed` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 41 | `response.mcp_list_tools.in_progress` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 42 | `response.code_interpreter_call.in_progress` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 43 | `response.code_interpreter_call.interpreting` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 44 | `response.code_interpreter_call.completed` | `output_index`, `item_id` | Accepted as hosted-tool lifecycle. |
| 45 | `response.code_interpreter_call_code.delta` | `output_index`, `item_id`, `delta` | Accepted; not incrementally normalized. |
| 46 | `response.code_interpreter_call_code.done` | `output_index`, `item_id`, `code` | Accepted; final item remains in terminal output. |
| 47 | `response.custom_tool_call_input.delta` | `output_index`, `item_id`, `delta` | Accepted and counted as output. |
| 48 | `response.custom_tool_call_input.done` | `output_index`, `item_id`, `input` | Pinned code emits an `x_search` started signal; result arrives through `output_item.done`. |
| 49 | `error` | optional `code`, `message`, optional `param` | Provider failure. |

`response.doom_loop_check` is a separate Grok Build proxy event consumed before this typed enum when the optional proxy feature is enabled. It is not one of the 49 Responses events and is not part of the public xAI API contract.

### Function tools and tool rounds

Public Responses function definitions are flat and use JSON Schema:

```json
{
  "type": "function",
  "name": "get_temperature",
  "description": "Get current temperature for a location",
  "parameters": {
    "type": "object",
    "properties": { "location": { "type": "string" } },
    "required": ["location"]
  }
}
```

The model returns a `function_call` output item. The caller executes it and supplies a `function_call_output` item with the matching `call_id`. Public examples use `previous_response_id`; Grok Build instead defaults to `store: false` and can replay prior response items, including encrypted reasoning, before the call and result. This matches hya's existing requirement that opaque Responses reasoning survive event storage, replay, and session forks (`.trellis/spec/backend/quality-guidelines.md:411-419`). [Function calling guide](https://docs.x.ai/developers/tools/function-calling)

Parallel calls are enabled by default in public documentation. All calls from a turn must be answered before continuation. `tool_choice` accepts `auto`, `required`, `none`, or a selected function. The public function guide says a streamed function call is whole in one chunk; delta assembly is necessary for Grok Build proxy compatibility but must also accept a complete call without deltas.

### Hosted web search and backend tools

Public xAI Responses enables server-side web search with:

```json
{
  "tools": [{ "type": "web_search" }]
}
```

The tool supports `filters.allowed_domains` or `filters.excluded_domains` (maximum five, mutually exclusive), plus `enable_image_understanding` and `enable_image_search`. The REST endpoint also accepts top-level `search_parameters` for mode, source/date constraints, citations, and result count; it says these override `web_search_preview` compatibility input. Hosted tools execute on xAI's server, while function tools pause for the client. [Web search guide](https://docs.x.ai/developers/tools/web-search) [Function/tool combination guide](https://docs.x.ai/developers/tools/function-calling#combining-with-built-in-tools)

The Responses stream exposes web-search lifecycle via `response.web_search_call.*`; pinned Grok Build starts a backend-tool event on `in_progress` and obtains the complete search item from `response.output_item.done`. Usage can report `num_sources_used`, `num_server_side_tools_used`, and per-tool usage details. Text citations can also arrive as output-text annotations.

Two separate first-party Grok Build mechanisms must not be conflated:

1. Responses-native hosted tools (`web_search`, plus raw proxy-specific `x_search`) run inside the inference request.
2. Grok Build also defines a local function named `web_search` with `query` and optional `allowed_domains`; its implementation makes a separate Responses request using the catalog's `web_search` model.

At the pinned commit, the default catalog selects `grok-4.20-multi-agent` for the separate search model and marks the `grok-4.5` entry `supports_backend_search: false`, even though current public xAI documentation demonstrates hosted `web_search` with `grok-4.5`. The supplied endpoint's backend-search claim therefore needs route-specific observation; neither the catalog flag nor public service behavior alone proves the custom route's behavior. [Default models, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-models/default_models.json#L1-L38) [Local web-search client, pinned](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-tools/src/implementations/web_search/client.rs)

### Reasoning support matrix

| Scope | Supported/represented values | Default | Evidence and interpretation |
|---|---|---|---|
| Public `grok-4.5` | `low`, `medium`, `high` | `high` | Model-specific public contract. Reasoning cannot be disabled. [Reasoning guide](https://docs.x.ai/developers/model-capabilities/text/reasoning) |
| Pinned Grok Build catalog entry for `grok-4.5` | `low`, `medium`, `high` | `high` | Product-selectable menu; agrees with public model docs. [Catalog](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-models/default_models.json#L8-L38) |
| Pinned Grok Build generic `ReasoningEffort` type | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` | Generic enum default: `medium` | Cross-model/backend representation. All seven map unchanged into the Responses schema; this is not Grok 4.5 capability evidence. [Type and mapping](https://github.com/xai-org/grok-build/blob/3af4d5d39897855bdcc74f23e690024a5dc05573/crates/codegen/xai-grok-sampling-types/src/types.rs#L765-L810) |
| Public `grok-build-0.1` model | Reasoning: yes; no effort values published | Not documented | The model page identifies a 256,000-token context and aliases `grok-code-fast-1`, `grok-code-fast`, and `grok-code-fast-1-0825`, but no effort menu. [Grok Build 0.1](https://docs.x.ai/developers/models/grok-build-0.1) |
| Public `grok-4.20-multi-agent` (Grok Build's separate search model) | `low`, `medium`, `high`, `xhigh` | Public guide does not state one in its summary row | Here effort controls agent count, not reasoning depth; this is not Grok 4.5 behavior. [Reasoning guide](https://docs.x.ai/developers/model-capabilities/text/reasoning#multi-agent-model) |

For the task's Grok 4.5 endpoint, primary evidence establishes exactly three effort probes: `low`, `medium`, and `high`. Sending `none`, `minimal`, `xhigh`, or `max` would test undocumented behavior rather than an established supported value.

Reasoning continuation details:

- Request `include: ["reasoning.encrypted_content"]` when using `store: false` and replay the opaque value unchanged in later turns.
- Stream both `response.reasoning_text.delta` and `response.reasoning_summary_text.delta` into the normalized reasoning channel.
- Preserve terminal reasoning items even when only a summary is displayable; they are protocol context for later function-call rounds.
- Reasoning models reject `presence_penalty`, `frequency_penalty`, and `stop` according to the current reasoning guide.
- Longer request/idle timeouts are expected for reasoning streams.

### Model and context metadata

| Source | Model | Context | Other relevant facts |
|---|---|---:|---|
| Public xAI model page | `grok-4.5`; aliases `grok-4.5-latest`, `grok-build-latest` | 500,000 | Function calling, structured output, and reasoning supported. |
| Pinned Grok Build catalog | `grok-4.5`, Responses backend | 500,000 | High/medium/low effort menu; `store: false` behavior comes from client defaults. |
| Public xAI model page | `grok-build-0.1`; aliases `grok-code-fast-1`, `grok-code-fast`, `grok-code-fast-1-0825` | 256,000 | Function calling, structured output, and reasoning supported; effort values not documented. |
| Task-supplied route claim | `grok-4.5`, Responses-style | 1,000,000 | Backend search claimed; neither context nor search was observed in this research. |

Pinned Grok Build can accept endpoint response headers `x-grok-context-window` and `x-grok-max-completion-tokens`, which are stronger route-specific evidence than a hard-coded public model value when present. The task requires sanitized live validation before treating the supplied 1,000,000 value as observed.

### Code Patterns

The project already has the intended extension boundary:

> "Provider behavior stays behind `Protocol::encode(CompletionRequest)` and a protocol-specific `Decoder` selected by `HttpProvider` construction." (`.trellis/spec/backend/quality-guidelines.md:404-405`)

The existing contract already states `/chat/completions` versus `/responses`, stateless Responses input, `store: false`, nested reasoning, all seven generic effort tokens, opaque reasoning replay, and failure normalization (`.trellis/spec/backend/quality-guidelines.md:409-427`). The Grok-specific evidence narrows model capability without requiring a new transport shape:

- Responses encoding is the base protocol.
- Grok 4.5 model metadata should expose only `low`, `medium`, and `high`, defaulting to `high`.
- The decoder must accept typed text, reasoning, tool, usage, terminal, and error events needed by hya; first-party Grok Build's larger 49-event decoder is compatibility evidence for accepted no-op lifecycle events.
- Endpoint-specific context/search metadata should remain route data, not a global Grok 4.5 constant.

### External References

- [xAI combined Chat and Responses REST reference](https://docs.x.ai/developers/rest-api-reference/inference/chat) - endpoints, complete current request fields, response objects, storage, usage, and examples.
- [xAI text reasoning](https://docs.x.ai/developers/model-capabilities/text/reasoning) - Grok 4.5 effort values/default, encrypted reasoning, summarized reasoning stream names, and reasoning restrictions.
- [xAI streaming](https://docs.x.ai/developers/model-capabilities/text/streaming) - SSE transport, `stream: true`, `[DONE]`, and timeout guidance.
- [xAI function calling](https://docs.x.ai/developers/tools/function-calling) - flat Responses tools, call/result loop, parallel calls, and whole-call streaming warning.
- [xAI web search](https://docs.x.ai/developers/tools/web-search) - Responses-native web tool and filters.
- [xAI Grok 4.5 model](https://docs.x.ai/developers/models/grok-4.5) - current model identity, aliases, capabilities, and 500,000-token context.
- [xAI Grok Build 0.1 model](https://docs.x.ai/developers/models/grok-build-0.1) - model identity, aliases, capabilities, and 256,000-token context.
- [Grok Build pinned source](https://github.com/xai-org/grok-build/tree/3af4d5d39897855bdcc74f23e690024a5dc05573) - exact first-party product request and normalization behavior.
- [Pinned Responses event schema](https://github.com/our-forks/async-openai/blob/95b52ebdedf42143083cf3d6f0e0be7c84e9c808/async-openai/src/types/responses/stream.rs) - exact 49 JSON event tags accepted by Grok Build's pinned dependency.

### Related Specs

- `.trellis/spec/backend/quality-guidelines.md:391` - OpenAI protocol selection, Responses request shape, generic effort encoding, opaque reasoning replay, failure behavior, and required tests.
- `.trellis/spec/frontend/quality-guidelines.md:81` - provider/model identity and per-model reasoning-variant behavior in the current TUI.
- `.trellis/tasks/07-21-grok-build-provider/prd.md:1` - feature goal, supplied route metadata, credential restriction, and acceptance criteria.

## Caveats / Not Found

- No live inference request succeeded, so no successful response headers, accepted effort values, search event, context window, or terminal stream shape is recorded here.
- The supplied credential was not read into this report, copied, printed, or committed.
- Public xAI documentation does not provide one exhaustive Responses SSE event list. The 49-event table is the exact decoder surface of Grok Build's pinned dependency, not a claim that the public service emits every event.
- Current REST field descriptions say `reasoning.effort` is "only supported by grok-4.3," while the model-specific reasoning page explicitly documents Grok 4.5 low/medium/high support. The model-specific page is the applicable capability evidence.
- The REST reference says a maximum of 128 tools; the function-calling guide's schema section says 200. No conclusion above relies on more than 128.
- Public function calls are documented as whole in one streamed chunk. `stream_tool_calls: true`, raw `x_search`, doom-loop events, and `x-grok-*` tracking headers are pinned Grok Build proxy extensions.
- Grok Build 0.1 is documented as a reasoning model, but no first-party public source found here specifies its accepted effort values or default.
- The route's advertised 1,000,000-token context and backend search support conflict with or extend current Grok Build catalog metadata. They remain endpoint claims pending sanitized observation.
