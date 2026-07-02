# Providers

The provider layer lives in [`../../crates/hya-provider`](../../crates/hya-provider).
It converts upstream LLM APIs into hya's canonical event stream.

## Core Traits

[`lib.rs`](../../crates/hya-provider/src/lib.rs) defines:

| Type | Purpose |
| --- | --- |
| `Provider` | A route that can stream a `CompletionRequest` for supported models. |
| `Protocol` | Encoder/decoder pair for one upstream API shape. |
| `Decoder` | Incrementally converts SSE frame data into `Event`s. |
| `Capabilities` | Route features such as streaming tool call support. |
| `CompletionRequest` | Normalized request containing model, system prompt, messages, tools, sampling options, reasoning effort, and request headers. |

`preflight` rejects tool-using requests if the chosen route does not support
streaming tool calls.

## Provider Router

[`router.rs`](../../crates/hya-provider/src/router.rs) keeps an ordered list of
providers. It resolves a request by asking each provider whether it has
capabilities for the requested `ModelRef`.

If no route supports the model, the router returns `UnknownModel`.

## HTTP Provider

[`http.rs`](../../crates/hya-provider/src/http.rs) is the shared live-provider
implementation. It owns:

- reqwest client
- upstream endpoint
- auth headers
- protocol encoder/decoder
- served model ids
- static capability metadata

Security details:

- redirects are disabled
- connect timeout is set
- auth headers are marked sensitive
- Anthropic keys use `x-api-key` and `anthropic-version`
- OpenAI-compatible keys use `Authorization: Bearer`
- Google keys use `x-goog-api-key`; the provider appends
  `/v1beta/models/<model>:streamGenerateContent?alt=sse` to the configured base
  URL

The response body is read as SSE. Each frame is sent into the protocol decoder,
and decoded events are forwarded through a channel as an `EventStream`.

## OpenAI-Compatible Protocol

[`openai.rs`](../../crates/hya-provider/src/openai.rs) encodes requests for
Chat Completions compatible APIs:

- system prompts become `role: system`
- tools become `type: function` tool definitions
- tool results are emitted as `role: tool`
- streamed text deltas become `TextStart` / `TextDelta` / `TextEnd`
- streamed tool arguments are accumulated and emitted as `ToolCallRequested`
- finish reasons map to hya `FinishReason`

Stored assistant messages may contain interleaved text and tool parts. The
encoder clusters `text + tool calls + results` into wire messages that satisfy
the provider's tool-call pairing rules.

## Anthropic Protocol

[`anthropic.rs`](../../crates/hya-provider/src/anthropic.rs) encodes requests
for Anthropic Messages:

- system prompt is placed in the top-level `system` field
- tools use Anthropic `input_schema`
- assistant `tool_use` blocks are paired with following user `tool_result`
  blocks
- `stop_reason: tool_use` maps to `FinishReason::ToolCalls`
- `stop_reason: max_tokens` maps to `FinishReason::Length`

Like the OpenAI decoder, the Anthropic decoder converts provider-specific
stream events into the same hya event variants.

## Google Protocol

[`google.rs`](../../crates/hya-provider/src/google.rs) encodes requests for
Gemini:

- system prompts become `systemInstruction`
- user text and canonical media parts become `contents[].parts`
- image, video, and audio data are passed as validated base64 `inlineData`
- tools become Gemini function declarations
- tool results become `functionResponse` parts
- reasoning effort maps to Gemini thinking-budget settings

The decoder maps streamed text, function calls, and finish reasons into hya's
canonical event variants.

## Fake and Dev Providers

Two non-live providers support development and tests:

- [`FakeProvider`](../../crates/hya-provider/src/fake.rs) replays scripted
  `FakeStep`s and is used by tests.
- [`DevProvider`](../../crates/hya-provider/src/dev.rs) echoes the latest user
  prompt and is used by the CLI when no live config is available.

The dev provider intentionally responds on every turn so multi-turn flows remain
usable without API keys.

## CLI Configuration

`hya-backend` builds routes from `~/.config/hya/config.yaml`. Provider ids and
models are surfaced through `hya-backend models`, Compat-compatible provider/model
HTTP routes, and saved-token auth commands. See
[`../configuration.md`](../configuration.md) for the YAML shape.
