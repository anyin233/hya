# Design: OpenAI Responses API and startup reasoning defaults

## 1. Decisions

- Keep the existing `Protocol` plus shared HTTP/SSE transport. Add one sibling
  `OpenAiResponsesProtocol`; do not branch the session engine or add a runtime
  channel.
- Keep Chat Completions as the compatibility default. The config `kind` values
  are `openai-completion` and `openai-response`; `openai` and
  `openai-compatible` remain aliases for `openai-completion`.
- Put reasoning metadata on model entries, not providers. Startup resolves the
  selected model's default and stores it on the base `AgentSpec`.
- Persist only a completed opaque Responses reasoning item on the existing
  reasoning part lifecycle. This is the smallest state needed for stateless tool
  continuation after replay or restart.

## 2. Data Flow

```text
config.yaml provider.kind + model reasoning metadata
  -> config::ResolvedConfig / ModelEntry
  -> runtime::resolve_runtime(selected model)
  -> AgentSpec.reasoning
  -> CompletionRequest.reasoning
  -> OpenAiChatProtocol: reasoning_effort (legacy mapping unchanged)
     OpenAiResponsesProtocol: reasoning.effort (exact seven-level mapping)

Responses SSE reasoning output item
  -> Event::ReasoningEnd.provider_data
  -> PartProjection::Reasoning.provider_data
  -> Part::Reasoning.provider_data
  -> next Responses input item, unchanged
```

The active startup model is chosen after CLI and `HYA_MODEL` overrides. Only a
matching configured model supplies a default; an unknown override remains
unset. Existing explicit Compat variants still override the base agent effort.

## 3. Configuration Contract

Both model syntaxes remain valid:

```yaml
providers:
  gateway:
    kind: openai-response
    base_url: https://gateway.example/v1
    models:
      - id: gpt-5.6-sol
        reasoning:
          default: medium
          variants: [none, minimal, low, medium, high, xhigh, max]
      - gpt-5.5
```

Implementation shape:

- Deserialize `models` as an untagged string/detailed enum.
- Normalize both forms into parsed model records before constructing the HTTP
  provider and public `ModelEntry` values.
- Detailed `reasoning.variants` replaces the provider-kind defaults when
  present. Missing variants use `ProviderKind::reasoning_variants()`.
- Parse every configured variant and default with `ReasoningEffort::parse`.
  Reject unknown values and reject an explicit non-`none` default absent from
  the advertised variants.
- Resolve a missing default with the existing
  `resolve_default_reasoning(None, None, variants)` policy. Empty variants mean
  no reasoning support and no default.
- `OpenAiResponse` advertises all seven empirically accepted levels. Existing
  Chat, Anthropic, and Google variant lists remain unchanged.

This deliberately changes legacy reasoning-capable string entries from no
startup effort to the existing highest-supported fallback. That is the requested
startup-default behavior; their API route and YAML syntax do not change.

## 4. Provider Selection

Extend `ProviderKind` with `OpenAiResponse`; keep `OpenAiCompatible` as the Chat
variant to avoid broad renaming. `HttpProvider::new` selects:

| Kind | Protocol | Endpoint |
| --- | --- | --- |
| `OpenAiCompatible` | `OpenAiChatProtocol` | `{base}/chat/completions` |
| `OpenAiResponse` | `OpenAiResponsesProtocol` | `{base}/responses` |
| Anthropic/Google | existing | existing |

No transport or auth changes are required.

## 5. Responses Request Contract

The Responses encoder emits:

```json
{
  "model": "gpt-5.6-sol",
  "instructions": "...",
  "input": [],
  "tools": [],
  "reasoning": {"effort": "medium", "summary": "auto"},
  "stream": true,
  "store": false
}
```

- Effort uses `ReasoningEffort::as_str()` exactly, including `none` and `max`.
  Chat continues to omit `Off` and clamp `Max` to `xhigh` through
  `openai_label()`.
- Function schemas use the flat Responses shape: `type`, `name`, `description`,
  `parameters`.
- User/system/assistant text becomes Responses input messages.
- Completed tool parts become a synthetic `function_call` followed by
  `function_call_output`, both using hya's stable internal `ToolCallId`.
- A reasoning part with `provider_data` is inserted unchanged before its
  function call. Live probes proved that synthetic function call IDs are
  accepted, so upstream response and call IDs do not need separate storage.
- Media remains rejected consistently with the current Chat encoder.

## 6. Event And Replay Contract

Add this backward-compatible optional field to existing types:

```rust
provider_data: Option<serde_json::Value>
```

Locations:

- `Event::ReasoningEnd`
- `PartProjection::Reasoning`
- `Part::Reasoning`

Use `serde(default, skip_serializing_if = "Option::is_none")`. Existing
providers and old logs use `None`. The projection reducer stores the value on
`ReasoningEnd`; core message reconstruction preserves reasoning parts instead of
dropping them. Compat renderers continue exposing only visible reasoning text.
Session forking copies the optional value so a fork can continue the same tool
round.

No new event variant, table, migration, provider-data registry, or parallel
projection is needed.

## 7. Responses SSE Mapping

The decoder keys assemblies by Responses `output_index` and emits the canonical
event lifecycle:

| Responses event | Canonical result |
| --- | --- |
| `response.output_text.delta` / `.done` | `TextStart`, `TextDelta`, `TextEnd` |
| `response.reasoning_summary_text.delta` | `ReasoningStart`, `ReasoningDelta` |
| reasoning `response.output_item.done` | `ReasoningEnd { provider_data }` |
| function `response.output_item.added` | `ToolInputStart` |
| `response.function_call_arguments.delta` | `ToolInputDelta` |
| function `response.output_item.done` | one `ToolCallRequested` |
| `response.completed` | usage plus `MessageFinished` |
| `response.incomplete` | `MessageFinished(Length)` |
| `response.failed` | `ProviderError` from nested error message |

Usage maps `input_tokens`, `output_tokens`, cached input tokens, and reasoning
output tokens into `TokenUsage`. Unknown semantic event types are ignored.
Duplicate terminal events are idempotent, matching the Chat decoder behavior.

## 8. Verification And Boundaries

- Local HTTP tests assert endpoint, request JSON, all seven effort labels, the
  full text/reasoning/tool/usage event sequence, nested failure handling, and a
  second request containing replayed opaque reasoning data.
- Projection/core tests assert provider data survives serde, replay, message
  reconstruction, and fork copying.
- Config/runtime tests assert aliases, invalid kinds/efforts, legacy model
  syntax, explicit `medium`, fallback defaults, and the first request effort.
- Existing Chat request tests remain unchanged and prove compatibility.
- A final live `gpt-5.6-sol` smoke checks the implemented Responses path. The
  seven-level gateway probe is already complete and recorded in `findings.md`.

Out of scope: Responses conversation storage, `previous_response_id`, non-stream
Responses, reasoning `mode`, provider-specific arbitrary option passthrough,
automatic default changes after a session model switch, and UI changes.

## 9. Planner Reconciliation

All four planners converged on a sibling protocol, explicit route selection,
startup model defaults, local SSE fixtures, and no new dependency. Resolutions:

- Existing `kind` is the selector instead of adding a second `api` field.
- Model defaults stay explicit/configurable; no model-name inference table.
- A frontier proposal to retain complete provider response batches was reduced
  to the completed reasoning item after live exact/synthetic/no-reasoning
  continuation probes showed response and upstream call IDs are unnecessary.
- Event compatibility uses an optional `ReasoningEnd` field instead of a new
  storage path or provider-specific event family.
