# Findings

- `HttpProvider::new` currently maps `ProviderKind::OpenAiCompatible` to
  `OpenAiChatProtocol` and `{base}/chat/completions`.
- `HttpProvider::stream` delegates request serialization and SSE decoding to a
  protocol object, so API selection likely belongs at provider construction
  rather than in the session engine.
- `HttpProvider` advertises reasoning support and derives catalog variants from
  `ProviderKind::reasoning_variants()`; the exact variant/default config path
  still needs inspection.
- `CompletionRequest` already has `reasoning: Option<ReasoningEffort>` and
  `OpenAiChatProtocol::encode` already emits `reasoning_effort` when that field
  is `Some`; the reported missing effort is therefore upstream request
  construction/model-variant resolution, not Chat Completions JSON encoding.
- OpenAI-compatible variants are currently hard-coded as `minimal`, `low`,
  `medium`, `high`, and `xhigh`. `ReasoningEffort` also represents `none` and
  `max`, with OpenAI mapping `max` to `xhigh`.
- Config deserializes provider kinds as `openai` (alias
  `openai-compatible`), `anthropic`, or `google`. It has no explicit OpenAI API
  selector, and `load()` passes only provider kind/base URL/key/models to
  `HttpProvider::new`.
- `model_entries()` publishes reasoning variants for every model based only on
  provider kind. The model entry shown so far has no default variant field.
- Responses support should reuse the existing `Protocol` boundary: provider
  construction chooses protocol plus endpoint, while `HttpProvider::stream`
  remains the shared HTTP/SSE transport.
- `request_from_messages()` copies only `agent.reasoning` into
  `CompletionRequest.reasoning`; it selects the active model independently but
  does not interpret the model's `#variant` or catalog variants there.
- The existing Compat resolver intentionally returns `None` when neither an
  agent variant, model variant, nor reasoning option is present. The previous
  reasoning-variants task explicitly required this old default, so this task is
  a deliberate contract change rather than a missing branch in that resolver.
- `resolve_default_reasoning(explicit, last_used, supported)` already expresses
  precedence and otherwise chooses the highest supported effort, but its actual
  native startup caller/ownership still needs confirmation.
- The prior Compat research records the Responses wire shape as
  `reasoning: { effort, summary }`, unlike Chat Completions'
  `reasoning_effort`. Current OpenAI documentation must be checked before fixing
  the exact request and SSE contracts.
- `ModelEntry` currently contains only id/provider/reasoning-variant names. It
  does not carry a configured default effort.
- No production caller of `resolve_default_reasoning()` surfaced in the indexed
  call graph; it currently appears to be an unused policy helper rather than a
  startup mechanism. Confirm with the exact startup symbols before design.
- Native server prompts use `ServerState.agent` directly, while Compat prompts
  call `session_agent_with_guidance()`. A native-config default must therefore
  be present on the base `AgentSpec`, with the Compat overlay still able to
  replace it for explicit variants/options.
- `OpenAiChatDecoder` currently maps text, function calls, finish reason, and
  usage only. It does not handle a Responses event stream (and no existing
  Responses event names were found), so Responses needs its own protocol
  encoder/decoder behind the existing trait.
- The earlier reasoning-variants task reports implementation and tests complete
  but its Trellis metadata remains `in_progress`; this task builds on the code,
  not on that stale lifecycle state.
- Current OpenAI reasoning docs recommend Responses for reasoning models and use
  `POST /v1/responses` with `reasoning: { "effort": <level> }`. They state the
  supported effort subset and default are model-dependent; GPT-5.6 defaults to
  `medium` when omitted, but hya still needs an explicit configured default per
  the requested startup contract.
- Current docs name the possible effort vocabulary as `none`, `minimal`, `low`,
  `medium`, `high`, `xhigh`, and `max`, but this is not proof every value is
  accepted by `gpt-5.6-sol`; use its model page and live provider checks to lock
  the test matrix.
- The official `gpt-5.6-sol` model page confirms both `/v1/chat/completions` and
  `/v1/responses`, streaming, and function calling. It does not enumerate the
  accepted effort subset, so the exact advertised matrix still requires a live
  boundary probe; GPT-5.6 docs state an omitted effort defaults to `medium`.
- A live boundary probe against the configured
  `https://api.12th.day/v1/responses` route on 2026-07-21 returned HTTP 200 and
  `status: completed` for every `gpt-5.6-sol` effort: `none`, `minimal`, `low`,
  `medium`, `high`, `xhigh`, and `max`. This is the acceptance-test matrix; the
  existing OpenAI family list omits `none`/`max` and `openai_label` clamps `max`
  to `xhigh`, so Responses needs model-configured variants and a wire mapping
  that can preserve `max`.
- GPT-5.6 `reasoning.mode` (`standard`/`pro`) is independent of effort. Mode was
  not requested and has no current domain field, so adding it is out of scope;
  Responses should send only the configured effort (and reasoning summary if
  needed for the existing visible reasoning event contract).
- Official Responses function tools are flat objects (`type`, `name`,
  `description`, `parameters`) rather than Chat's nested `function` object;
  outputs return `function_call` items and subsequent requests use
  `function_call_output` with the same `call_id`.
- OpenAI explicitly requires reasoning output items from a reasoning model's
  tool-call response to be passed back with the function outputs. Before calling
  Responses tool support complete, verify whether the event-sourced `Part` model
  can preserve the opaque reasoning item; otherwise the design needs the
  smallest event-backed representation rather than dropping it.
- The verification found that `Part::Reasoning` and `PartProjection::Reasoning`
  currently store only visible text, while
  `engine/turn/messages.rs::map_parts()` explicitly drops reasoning parts from
  the next `CompletionRequest`. Responses cannot round-trip opaque reasoning
  state without a small backward-compatible event/projection/part extension.
- A live stateless (`store:false`) high-effort function-call stream produced a
  `response.output_item.added`/`done` item of type `reasoning` with keys
  `content`, `encrypted_content`, `id`, `summary`, and `type`, followed by a
  `function_call` item with `arguments`, `call_id`, `id`, `name`, `status`, and
  `type`. Preserve the completed reasoning item as event-backed provider data
  and replay it unchanged before the matching `function_call` and
  `function_call_output` items.
- The same live stream confirmed summary events
  `response.reasoning_summary_part.added`,
  `response.reasoning_summary_text.delta`/`done`, and
  `response.reasoning_summary_part.done`; these can map to the existing visible
  `ReasoningStart`/`Delta`/`End` lifecycle while the opaque completed item is
  retained separately on that reasoning part.
- Responses streaming uses semantic JSON events including
  `response.output_text.delta`, function-call argument delta/done events,
  `response.completed`, `response.failed`, and `error`. The JSON object itself
  contains `type`, so the current SSE pump can keep passing only `frame.data`.
- Official sources consulted:
  `https://developers.openai.com/api/docs/guides/reasoning` and
  `https://developers.openai.com/api/docs/guides/streaming-responses`.
- `ProviderConfig` currently has only `kind`, `base_url`, `api_key`, and a flat
  `models: Vec<String>`; `load()` resolves this into `ParsedProvider`, then calls
  `HttpProvider::new(...)`. API choice therefore needs one config value carried
  through those existing structs into provider construction.
- `model_entries()` derives every model's advertised reasoning variants solely
  from `ProviderKind`; `ModelEntry` has no default effort and the flat model list
  has nowhere to configure one. The configured default needs an explicit model
  metadata path before runtime can put it on the base `AgentSpec`.
- Existing config tests assert OpenAI-compatible aliases and provider-wide
  variants (`minimal`, `low`, `medium`, `high`, `xhigh`); these are direct
  regression points for preserving existing configs and refining per-model
  defaults.
- `hya-app::runtime::agent_with_model()` currently hard-codes
  `AgentSpec.reasoning = None`; this is the native startup root cause for the
  first request omitting effort even though Chat Completions can serialize it.
- `agent_with_model()` accepts only a model string. `ResolvedConfig` already
  returns both `default_model` and `models`, so startup can resolve the chosen
  model's configured default before constructing the base `AgentSpec` without
  changing the engine or server contracts.
- The existing flat `models: Vec<String>` syntax has no model metadata slot.
  A per-model default either requires a backward-compatible untagged
  string/object model entry or a separate provider-level/model-keyed field;
  repository docs and the prior reasoning task must decide which is native.
- The HTTP integration test already captures endpoint, auth, request JSON, and
  decoded events from a local SSE server. Extending this one test surface is the
  smallest executable proof for both API choices and Responses event mapping.
- Existing absent API configuration posts to `/chat/completions`; preserving it
  as the default is required by the explicit no-silent-reinterpretation
  constraint. A configured Responses choice can be additive at provider
  construction.
- `docs/configuration.md` documents only flat model strings and says `kind:
  openai`/`openai-compatible` means Chat Completions. Therefore existing `kind`
  values must retain Chat behavior; the Responses selector should be a new
  optional provider field whose absent value is Chat Completions.
- The prior reasoning-variants design explicitly left
  `agent_with_model()` at `None` and excluded native default selection. This task
  supersedes only that exclusion; the Compat per-turn resolver remains the
  explicit session/agent variant override path.
- `OpenAiChatProtocol` already contains reusable message/tool-history semantics,
  but Responses requires different item shapes (`input`, `function_call`, and
  `function_call_output`) and therefore should be a sibling protocol rather than
  branches inside the Chat encoder.
- The shared SSE pump catches transport errors and frames with a top-level
  `error`, then delegates frame data to `Decoder`. A Responses decoder must
  itself turn `response.failed` (whose error is nested under `response`) into
  `ProviderError`; no pump or trait change is needed.
- `OpenAiChatDecoder` demonstrates the existing event lifecycle to preserve:
  start/delta/end for text and tool input, one `ToolCallRequested` per completed
  call, then `MessageFinished` with finish reason and optional token usage.
- `resolve_default_reasoning()` has tests but no production caller. Its current
  fallback selects the strongest supported level, which is not necessarily the
  user-configured default requested here; reuse only its precedence if the final
  config contract needs it.
- No product-code changes have started; the Trellis task remains `planning`.
- Task 1 implementation trace confirmed `Projection::apply_event` currently
  ignores `ReasoningEnd`, `engine::turn::messages::map_parts` drops reasoning,
  and `SessionEngine::copy_text_part` recreates `ReasoningEnd` without opaque
  data. Existing provider constructors need explicit `None`; Compat renderers
  only need non-exhaustive `..` patterns so the value stays invisible.
- Task 1 is implemented and verified. `provider_data` is optional and omitted
  when absent on `ReasoningEnd`, `PartProjection::Reasoning`, and
  `Part::Reasoning`; legacy events deserialize it as `None`. The reducer stores
  completed data, message reconstruction retains it, and forks copy it through
  the normal reasoning lifecycle. Tasks 2-5 remain untouched.
- Task 2's first config test is GREEN. The parser can normalize string/object
  model entries, validate efforts with `ReasoningEffort::parse`, and resolve a
  default through `resolve_default_reasoning`; the remaining parser RED cases
  are unknown kind/effort, unsupported default, and legacy Chat aliases.
- Runtime inspection reconfirmed the single loss point:
  `agent_with_model()` hard-codes `reasoning: None`. `RuntimeConfig` is the
  existing startup value carrier, and `build_session_engine()` independently
  creates the team-supervisor base agent, so one selected reasoning field must
  reach both direct startup agents and that constructor.
- `ModelEntry::matches_model_ref()` already matches bare IDs and
  `provider/model` references, so runtime selection can reuse it directly.
  There are eight production `agent_with_model()`/`build_session_engine()`
  startup call sites across `hya-app`, backend exec/RPC/goal, serve, and TUI.
- Runtime tests already serialize environment changes with `EnvGuard` and load
  `${XDG_CONFIG_HOME}/hya/config.yaml`; this is the smallest integration seam
  for proving configured selection and first-agent construction together.
- Every startup constructor already has the resolved `RuntimeConfig` in scope.
  One `Option<ReasoningEffort>` argument covers direct agents and the team base;
  offline and tail-session paths have no configured model and use `None`.
- The existing provider HTTP behavior-contract suite captures endpoint, headers,
  JSON body, and decoded events from a one-request local SSE server. Responses
  can reuse it unchanged; the approved request uses `instructions`, `input`,
  flat tools, exact `ReasoningEffort::as_str()`, `summary:auto`, and `store:false`.
- Responses request and stream behavior fits entirely behind the existing
  `Protocol`/`Decoder` boundary. Output-index maps handle parallel semantic SSE;
  the shared HTTP pump needs no Responses branch.
- Continuation needs no provider call-ID storage: persisted reasoning data is
  replayed unchanged and the existing `ToolCallId` is sufficient for both the
  synthetic function call and its output.
