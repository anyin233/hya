# Providers

The provider layer lives in [`../../crates/hya-provider`](../../crates/hya-provider).
It converts upstream LLM APIs into hya's canonical event stream.

## Core Traits

[`lib.rs`](../../crates/hya-provider/src/lib.rs) defines:

| Type | Purpose |
| --- | --- |
| `Provider` | A route that claims models via `capabilities`, streams a `CompletionRequest` into canonical `Event`s, and optionally exposes a configured routing identity and native Responses compaction. |
| `Protocol` | Encoder/decoder pair for one upstream API shape. |
| `Decoder` | Incrementally converts SSE frame data into `Event`s. |
| `Capabilities` | Fixed capability flags and context budget advertised for a model claim (see [Capabilities](#capabilities)). |
| `CompletionRequest` | Normalized request containing model, system prompt, messages, tools, sampling options, reasoning effort, and request headers. |
| `ProviderError` | Encode, HTTP, resolve, decode, and auth-expiry failures (see [Errors](#errors)). |

`preflight` rejects tool-using requests if the chosen route does not support
streaming tool calls.

### `Provider` method contract

| Method | Default | Role |
| --- | --- | --- |
| `fn id(&self) -> &str` | required | Configured provider id. Also the auth filename stem and the `providerID` half of model refs. |
| `fn capabilities(&self, model: &ModelRef) -> Option<Capabilities>` | required | Returning `Some` **claims** the model. The router resolves by first match. |
| `fn configured_identity_v1(&self) -> Option<Vec<u8>>` | `None` | Secret-free routing fingerprint. Default fails closed (see [Configured Identity](#configured-identity)). |
| `fn catalog(&self) -> Vec<ProviderModel>` | empty `Vec` | Models this route publishes into the aggregated catalog. |
| `async fn stream(req, session, message) -> Result<EventStream, ProviderError>` | required | Live or scripted completion stream. |
| `async fn compact_responses(model, messages, system) -> Result<Option<CompactedWindow>, ProviderError>` | `Ok(None)` | Native `POST /responses/compact` when the route supports it; `None` means callers fall back to a local summarizer. |

Minimum surface for a new implementor: `id`, `capabilities`, and `stream`.

### `Protocol` and `Decoder`

```text
trait Protocol {
    fn encode(&self, req: &CompletionRequest) -> Result<serde_json::Value, ProviderError>;
    fn decoder(&self, session: SessionId, message: MessageId) -> Box<dyn Decoder>;
}

trait Decoder {
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError>;
    fn finish(&mut self) -> Result<Vec<Event>, ProviderError>;
}
```

`encode` builds the HTTP JSON body. `decoder` returns a fresh stateful decoder.
Each `push`/`finish` returns a batch of canonical `Event`s (may be empty).

### Capabilities

`Capabilities` has seven fields:

| Field | Meaning |
| --- | --- |
| `streaming_tool_calls` | Route may stream tool-call assembly mid-turn. |
| `parallel_tool_calls` | Multiple tool calls in one assistant turn are allowed. |
| `usage_reporting` | Stream may emit token usage on finish. |
| `json_output` | Structured JSON output mode (not used by current HTTP defaults). |
| `reasoning_stream` | Provider streams separate reasoning parts as first-class stream events (flag only; HTTP default is off). |
| `reasoning_request` | Route accepts a reasoning-effort parameter on the request. |
| `max_context` | Advertised context window (tokens). |

**HTTP default** (`HttpProvider::new`, every kind and model):

- `streaming_tool_calls` = true
- `parallel_tool_calls` = true
- `usage_reporting` = true
- `reasoning_request` = true
- `json_output` = false
- `reasoning_stream` = false
- `max_context` = **200_000**

There is **no** per-model capability table. Every configured HTTP route reports
the same caps for every model it serves. The context window surfaced by
`GET /api/model` (and similar catalog views) is therefore this fixed **200k**
default, not the model's real limit.

`DevProvider` claims the same set **minus** `reasoning_request` (left false via
`Capabilities::default()`), which is why it accepts any `ModelRef`.
`FakeProvider` uses the same pattern as Dev for tool/usage/context flags and also
leaves `reasoning_request` false.

### Errors

`ProviderError` variants and their display prefixes:

| Variant | Display | Typical cause | Troubleshooting |
| --- | --- | --- | --- |
| `Json` | `json: …` | Serde failure while encoding a body or parsing a stream frame. | No dedicated entry; inspect the JSON payload and stream frames. |
| `Http(String)` | `http: …` | Non-2xx status, in-stream error frame, invalid request header name/value, or client build failure. | No dedicated entry; check status body, base URL, and headers. |
| `UnknownModel(String)` | `unknown provider for model: …` | No route's `capabilities()` returned `Some` for the ref. | [`../troubleshooting.md`](../troubleshooting.md) — *unknown provider for model*. |
| `Incompatible(String)` | `incompatible route: …` | Preflight failure, or an unsupported part (for example media on a non-Google route). | No dedicated entry yet; for media MIME failures see [Media parts](#media-parts-non-google-routes) below and switch to a `kind: google` route when you need attachments. |
| `Decode(String)` | `decode: …` | Malformed or truncated stream / compact-window payload. | No dedicated entry; inspect SSE frames. |
| `AuthExpired { provider, hint }` | `auth expired for provider '<p>': <hint>` | Produced by the OAuth bearer-resolver wiring in `hya-app` when refresh fails or credentials are revoked. | Re-run `hya-backend oauth login` for that provider (see [`../configuration.md`](../configuration.md) / CLI auth). |

## Provider Router

[`router.rs`](../../crates/hya-provider/src/router.rs) keeps an **ordered** list of
providers (insertion order via `with`).

**Resolution.** `resolve` returns the **first** provider whose
`capabilities(&model)` is `Some`. When two configured routes both serve the same
bare model id, the earlier insertion wins — the usual tie-break when two
OpenAI-compatible gateways list overlapping models.

If no route supports the model, `stream` returns `UnknownModel`.

**Reasoning strip.** Before dispatch, if the resolved route's capabilities do
**not** set `reasoning_request`, the router clears
`CompletionRequest.reasoning`. A configured `reasoning.default` is therefore
**silently dropped** (not an error) on routes that cannot accept a reasoning
parameter, so no unsupported field is sent upstream.

**Catalog.** `ProviderRouter::catalog` flattens every provider's `catalog()`,
sorts by `(provider_id, model_id)`, and dedups identical provider/model pairs.
That ordering is what `hya-backend models` and `GET /api/model` expose.

**Identities.** `configured_identities_v1` aggregates per-provider fingerprints
in insertion order, or returns `None` if **any** provider returns `None` or an
empty identity (fail closed). See [Configured Identity](#configured-identity).

## HTTP Provider

[`http.rs`](../../crates/hya-provider/src/http.rs) is the shared live-provider
implementation. It owns:

- reqwest client
- upstream endpoint (or Google base + per-request model path)
- auth style / headers
- protocol encoder/decoder
- served model ids
- static capability metadata
- optional bearer resolver and per-model reasoning variant lists

### Construction

```text
HttpProvider::new(id, kind, base_url, api_key, models)
```

builds one route. `ProviderKind` selects the protocol encoder/decoder, the
endpoint path, and the default auth style. A trailing `/` is trimmed from
`base_url` before the endpoint is built, so `https://host/v1` and
`https://host/v1/` behave identically.

Builder methods layered on top:

| Method | Effect |
| --- | --- |
| `with_model_reasoning_variants` | Per-model reasoning effort vocabulary. |
| `with_codex_session_auth` | Upgrade auth to Codex session headers (no-op unless kind is `OpenAiCodex`). |
| `with_grok_session_auth` | Upgrade auth to Grok session headers (no-op unless kind is `GrokBuild`). |
| `with_bearer_resolver` | Resolve the bearer token on each stream (hot-reload OAuth). |

Session-auth upgrades are no-ops for other kinds so callers can chain
unconditionally.

### Provider kinds and auth styles

Six `ProviderKind` values (not three):

| Kind | Protocol | Default endpoint | Default auth |
| --- | --- | --- | --- |
| `OpenAiCompatible` | Chat Completions | `{base}/chat/completions` | Bearer |
| `OpenAiResponse` | Responses | `{base}/responses` | Bearer |
| `OpenAiCodex` | Responses | `{base}/responses` | Bearer → may upgrade to `CodexSession` |
| `GrokBuild` | Responses (+ encrypted reasoning include) | `{base}/responses` | Bearer → may upgrade to `GrokSession` |
| `Anthropic` | Messages | `{base}/messages` | Anthropic (`x-api-key` + version) |
| `Google` | Gemini streamGenerateContent | built per model under `{base}` | Google (`x-goog-api-key`) |

Five auth styles: **Bearer**, **CodexSession**, **GrokSession**, **Anthropic**,
**Google**.

### Client policy and security

- **Redirects disabled** (`Policy::none`) so an `x-api-key` (or other secret
  header) cannot be forwarded cross-origin on a 3xx.
- **Connect timeout: 10 seconds.**
- **No read / total timeout** — a long streaming completion is never aborted
  mid-stream by the client. Consequence: a hung upstream that has already
  accepted the connection will not time out on hya's side.
- Auth header values are marked **sensitive** on `HeaderValue` so reqwest/tracing
  will not log them.
- Anthropic routes hardcode `anthropic-version: **2023-06-01**`. That value is
  **not** configurable through `config.yaml`; changing it requires a code change
  at `crates/hya-provider/src/http.rs` (Anthropic `AuthStyle` construction).
- OpenAI-compatible / Responses / Codex / Grok bearer routes use
  `Authorization: Bearer`.
- Google keys use `x-goog-api-key`; the provider appends
  `/v1beta/models/<model>:streamGenerateContent?alt=sse` to the configured base
  URL.

### Per-request extra headers

`CompletionRequest.headers` are merged **over** the route's auth headers on
every call — a plugin-supplied header of the same name **wins**. Every extra
value is marked sensitive. An invalid header name or value fails the call with
`ProviderError::Http` (`invalid request header name` / `invalid request header
value`) rather than being silently dropped.

The response body is read as SSE. Each frame is sent into the protocol decoder,
and decoded events are forwarded through a channel as an `EventStream`.

## OpenAI-Compatible Protocol

[`openai.rs`](../../crates/hya-provider/src/openai.rs) encodes requests for
Chat Completions compatible APIs (`ProviderKind::OpenAiCompatible`):

- system prompts become `role: system`
- tools become `type: function` tool definitions
- tool results are emitted as `role: tool`
- streamed text deltas become `TextStart` / `TextDelta` / `TextEnd`
- streamed tool arguments are accumulated and emitted as `ToolCallRequested`
- decoder closes on SSE data `[DONE]` or on plain stream end (`finish()`)

**Finish-reason mapping** (`openai/decoder.rs`):

| Upstream `finish_reason` | hya `FinishReason` |
| --- | --- |
| `tool_calls` | `ToolCalls` |
| `length` | `Length` |
| `content_filter` | `Error` |
| anything else, including absent | `Stop` |

**Null tool input.** A stored tool input that is JSON `null` is serialized as
the string `"{}"` in `function.arguments`, because the wire format requires a
JSON object string.

Stored assistant messages may contain interleaved text and tool parts. The
encoder clusters `text + tool calls + results` into wire messages that satisfy
the provider's tool-call pairing rules.

**Media.** User/system media parts fail encode with
`ProviderError::Incompatible("OpenAI chat does not support media type <mime>")`
(display: `incompatible route: …`). Assistant `Part::Media` entries are ignored
on encode (not forwarded). Only Google encodes media; see below.

## OpenAI Responses Protocol

Used by three kinds: **`openai-response`**, **`openai-codex`**, and
**`grok-build`**. Encoder/decoder live in
[`openai/responses.rs`](../../crates/hya-provider/src/openai/responses.rs) and
[`openai/response_decoder.rs`](../../crates/hya-provider/src/openai/response_decoder.rs).

### Encode shape

`OpenAiResponsesProtocol::encode` builds:

```json
{
  "model": "<id>",
  "input": [ /* items */ ],
  "tools": [ /* function tools */ ],
  "stream": true,
  "store": false
}
```

Optional fields when present on the request:

- `instructions` — from `CompletionRequest.system`
- `reasoning`: `{ "effort": "<level>", "summary": "auto" }`
- `temperature`
- `max_output_tokens`

`GrokBuildProtocol` wraps the same encoder and always adds
`include: ["reasoning.encrypted_content"]`.

`encode_input_items` is the shared public helper used by both the create path
and `/responses/compact`.

### Encrypted-reasoning replay

When re-emitting an assistant message that contains reasoning parts, the encoder
pushes each reasoning part's stored `provider_data` item **verbatim** into
`input`. That is how encrypted reasoning content survives multi-round turns
instead of being summarized away.

### Media

Any `Part::Media` in user or assistant history fails encode with
`ProviderError::Incompatible("OpenAI Responses does not support media type <mime>")`.

### Decoder

`OpenAiResponsesDecoder` keys reasoning, text, and tool assembly by
`output_index`, and tracks started / ended / requested state per part
(`PartAsm` / `ToolAsm`).

Handled event `type` values include:

| Event type | Behavior |
| --- | --- |
| `response.reasoning_summary_text.delta` / `response.reasoning_text.delta` | Reasoning delta |
| `response.output_item.added` (`item.type` = `function_call`) | Tool assembly start |
| `response.output_item.done` (`item.type` = `reasoning`) | Reasoning close + `provider_data` |
| `response.output_item.done` (`item.type` = `function_call`) | Tool call finalize |
| `response.function_call_arguments.delta` | Tool args delta |
| `response.output_text.delta` / `response.output_text.done` | Text stream |
| `response.completed` | Usage + finish (`ToolCalls` if any tool was seen, else `Stop`) |
| `response.incomplete` | Usage + `Length` |
| `response.failed` | `ProviderError::Http` from `/response/error/message` |
| bare `error` | `ProviderError::Http` from error message fields |

Grok Build uses a decoder variant that **requires** a typed terminal
(`response.completed` or `response.incomplete`); otherwise finish errors with a
missing-terminal message.

Responses kinds that support compact expose `POST {base}/responses/compact` via
`Provider::compact_responses`.

## Anthropic Protocol

[`anthropic.rs`](../../crates/hya-provider/src/anthropic.rs) encodes requests
for Anthropic Messages:

- system prompt is placed only in the top-level `system` field
- **`Message::System` rows in the message history are dropped** — they never
  become wire messages; only `CompletionRequest.system` reaches Anthropic
- tools use Anthropic `input_schema`
- assistant `tool_use` blocks are paired with following user `tool_result`
  blocks
- `stop_reason: tool_use` maps to `FinishReason::ToolCalls`
- `stop_reason: max_tokens` maps to `FinishReason::Length`

### `max_tokens` and thinking budget

The encoder always emits `{ model, messages, stream: true, max_tokens }` (plus
optional `system`, `tools`, `thinking`).

- When `CompletionRequest.max_output_tokens` is absent, **`max_tokens` defaults
  to 4096**, which silently caps output length on Anthropic routes.
- When a thinking budget is set from reasoning effort
  (`reasoning.anthropic_budget()`), the body includes
  `thinking: { type: "enabled", budget_tokens: <budget> }`, and `max_tokens` is
  raised to `budget + 4096` if the current value would not already exceed the
  budget (`max_tokens <= budget`).

### Media

User media parts fail with
`ProviderError::Incompatible("Anthropic messages does not support media type <mime>")`.
Assistant `Part::Media` entries are ignored on encode (not forwarded).

Like the OpenAI decoder, the Anthropic decoder converts provider-specific
stream events into the same hya event variants.

## Google Protocol

[`google.rs`](../../crates/hya-provider/src/google.rs) encodes requests for
Gemini:

- system prompts become `systemInstruction` (top-level system plus any
  `Message::System` history rows concatenated)
- user text and canonical media parts become `contents[].parts`
- tools become Gemini function declarations
- tool results become `functionResponse` parts
- reasoning effort maps to Gemini thinking-budget settings

### Inline media contract

**Accepted MIME types** (13-entry allowlist; any other MIME →
`ProviderError::Incompatible("Google does not support media type …")`):

- `image/png`, `image/jpeg`, `image/gif`, `image/webp`
- `video/mp4`, `video/webm`, `video/quicktime`
- `audio/wav`, `audio/mp3`, `audio/aiff`, `audio/aac`, `audio/ogg`, `audio/flac`

**Size caps:**

- **28 MiB** encoded (base64 character length)
- **20 MiB** decoded (raw bytes after base64 decode)

**Payload forms:**

- raw base64 payload, or
- a `data:<mime>;base64,<payload>` URL whose declared MIME in the header must
  match the part's declared MIME type (case-insensitive)

Non-canonical base64 (decoded then re-encoded differs) is rejected. Valid media
is sent as `inlineData: { mimeType, data }`.

### Decoder and finish reasons

The decoder reads **`candidates[0]` only**, coalesces all text parts into a
single text part, and closes on the first `finishReason` it sees.

| Condition | hya `FinishReason` |
| --- | --- |
| Any function call was seen in the stream | `ToolCalls` (forced, regardless of Gemini's reason) |
| else `MAX_TOKENS` | `Length` |
| else `SAFETY` or `RECITATION` | `Error` |
| else | `Stop` |

## Media parts (non-Google routes)

Canonical media parts (for example from v2 prompt file attachments) are only
encoded on the **Google** route. On OpenAI chat, OpenAI Responses / Codex /
Grok Build, and Anthropic, media in the positions those encoders validate fails
the turn with `ProviderError::Incompatible` and a
`… does not support media type <mime>` message (wrapped as
`incompatible route: …`). To send images or other attachments, use a session
route with `kind: google`.

## Usage Reporting

When `usage_reporting` is true (HTTP default), decoders fill `TokenUsage` from
protocol-specific fields. Always-zero fields are **not** measurements — the
upstream simply does not expose that slot on that wire.

| Protocol | input | output | reasoning | cache_read | cache_write |
| --- | --- | --- | --- | --- | --- |
| OpenAI chat | `prompt_tokens` | `completion_tokens` | `completion_tokens_details.reasoning_tokens` | `prompt_tokens_details.cached_tokens` | `prompt_tokens_details.cache_creation_tokens` |
| OpenAI Responses | `/response/usage` `input_tokens` | `output_tokens` | `output_tokens_details.reasoning_tokens` | `input_tokens_details.cached_tokens` | **always 0** |
| Anthropic | `input_tokens` | `output_tokens` | **always 0** | `cache_read_input_tokens` | `cache_creation_input_tokens` |
| Google | `usageMetadata.promptTokenCount` | `candidatesTokenCount` | `thoughtsTokenCount` | `cachedContentTokenCount` | **always 0** |

Live HTTP routes declare `usage_reporting: true`. Per-protocol usage frames feed
the store's token ledger (`record_usage` path) when non-zero usage is present on
finish.

## Configured Identity

`Provider::configured_identity_v1` returns a deterministic, **secret-free**
fingerprint of a route's configuration. Callers use it to detect that a
TurnBinding's provider config changed. Providers without a complete identity
**fail closed** by returning `None` (trait default).

### HTTP fingerprint contents

For `HttpProvider` the identity bytes include:

- tag `hya.provider.http.configured.v1`
- crate version (`CARGO_PKG_VERSION`)
- provider id
- kind tag (`openai-compatible`, `openai-response`, `openai-codex`, `grok-build`,
  `anthropic`, `google`)
- endpoint string
- optional Google base
- alias rules markers
- sorted model set
- per-model reasoning variants
- full `Capabilities` bits
- auth **shape** (style tag + non-secret fields)

**Deliberately excluded:** the token/API key itself. Auth contributes only:

- style tag (`bearer`, `codex-session`, `grok-session`, `anthropic`, `google`)
- a boolean “secret is non-empty” flag
- non-secret fields: Codex optional account id; Grok client version +
  identifier; Anthropic API version string

### Router aggregation

`ProviderRouter::configured_identities_v1` returns one identity per provider in
insertion order, or `None` if **any** provider returns `None` or an empty
vector. Putting a `FakeProvider` (default identity `None`) in the router fails
the whole set closed.

## Fake and Dev Providers

Two non-live providers support development and tests.

### FakeProvider

[`fake.rs`](../../crates/hya-provider/src/fake.rs) — id **`fake`**. Replays one
scripted step list per assistant turn (`scripted` / `scripted_turns`).

`FakeStep` vocabulary:

| Variant | Effect when materialized |
| --- | --- |
| `Text(String)` | `TextStart` / `TextDelta` / `TextEnd` |
| `Reasoning(String)` | `ReasoningStart` / `ReasoningDelta` / `ReasoningEnd` |
| `ToolCall { name, input }` | Tool input start/delta + `ToolCallRequested` |
| `Usage(TokenUsage)` | Merged into finish tokens |
| `Finish(FinishReason)` | `MessageFinished` with that reason |

**Termination.** Once the scripted turns are exhausted, every further `stream`
call emits a bare `Finish(Stop)` so agent loops terminate instead of hanging or
replaying a tool call forever.

```rust
use hya_provider::{FakeProvider, FakeStep};
let provider = FakeProvider::scripted(vec![
    FakeStep::Text("hello".into()),
    FakeStep::Finish(hya_proto::FinishReason::Stop),
]);
```

Because `configured_identity_v1` defaults to `None`, a `FakeProvider` in a
`ProviderRouter` makes `configured_identities_v1` return `None` for the whole
set.

### DevProvider

[`DevProvider`](../../crates/hya-provider/src/dev.rs) echoes the latest user
prompt and is used by the CLI when no live config is available. It claims every
model (`capabilities` always `Some`) without `reasoning_request`. The dev
provider intentionally responds on every turn so multi-turn flows remain usable
without API keys.

## CLI Configuration

`hya-backend` builds routes from `~/.config/hya/config.yaml`. Provider ids and
models are surfaced through `hya-backend models`, Compat-compatible provider/model
HTTP routes, and saved-token auth commands. See
[`../configuration.md`](../configuration.md) for the YAML shape.
