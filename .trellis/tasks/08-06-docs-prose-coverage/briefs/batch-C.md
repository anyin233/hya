# Batch C - providers.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/architecture/providers.md`

You have **18 gap entries** and **2 stale claims** to resolve.

## Non-negotiable rules

1. **Confirm every claim against the source before you write it.** Every entry
   below carries a `source` reference. Open it. If the source contradicts the
   entry, the SOURCE WINS -- write what the code does and report the discrepancy.
2. **If you cannot confirm a claim from source, do not write it.** Say you could
   not confirm it. Plausible prose that is wrong is worse than an admitted gap,
   because a reader trusts the document.
3. **Stale and contradicted entries are corrected or deleted, never merely
   supplemented.** A document that contradicts the code is a defect.
4. **Do not edit any file outside your batch.** Other writers are working in
   parallel. In particular never touch `docs/README.md`, `README.md`, `AGENTS.md`,
   `DESIGN.md`, or `docs/project-structure.md` -- a later reconciliation pass owns
   all cross-links and the docs map. Some entries below suggest edits to other
   files; ignore that part and write only your own.
5. **Match the existing documentation style.** Read the file you are editing
   before writing. Use the project's vocabulary as defined in `CONTEXT.md`.
6. **A feature counts as documented only if a reader can use it** from what you
   write: what it does, its parameters or keys, and its semantics. A name in a
   list does not count. 11 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/architecture/providers.md`

**1. OpenAI Responses / Codex / Grok Build protocol (encoder, decoder, event vocabulary, encrypted-reasoning replay)** — `undocumented` · severity high

- Source: `crates/hya-provider/src/openai/responses.rs:34, crates/hya-provider/src/openai/response_decoder.rs:51`
- Evidence: docs/architecture/providers.md has sections for "OpenAI-Compatible Protocol", "Anthropic Protocol" and "Google Protocol" only — there is no Responses section. Grepping docs for `responses.rs`, `OpenAiResponsesProtocol`, `output_index`, `output_item.added` returns nothing. docs/configuration.md:196 only says grok-build "uses the Responses request shape and adds encrypted reasoning content".
- Write: Add a `## OpenAI Responses Protocol` section between the OpenAI-Compatible and Anthropic sections, covering the three kinds that use it (`openai-response`, `openai-codex`, `grok-build`). Document: the encode shape `{model, input, tools, stream:true, store:false}` plus optional `instructions`, `reasoning:{effort, summary:"auto"}`, `temperature`, `max_output_tokens` (responses.rs:34); that GrokBuildProtocol wraps the same encoder and adds `include: ["reasoning.encrypted_content"]` (responses.rs:18); that `encode_input_items` is the shared public helper used by both the create and compact paths (responses.rs:82); and the key replay rule — assistant reasoning parts are re-emitted verbatim from their stored `provider_data` item so encrypted reasoning survives multi-round turns (responses.rs:160). Then document the decoder: it keys reasoning/text/tool parts by `output_index` and tracks started/ended/requested state per part (response_decoder.rs:51), and list the handled event names — `response.reasoning_summary_text.delta`, `response.reasoning_text.delta`, `response.output_item.added/done` (function_call, reasoning), `response.function_call_arguments.delta`, `response.output_text.delta/done`, `response.completed/incomplete/failed`, and a bare `error` frame (response_decoder.rs:347).

**2. Media parts are rejected by every non-Google route** — `undocumented` · severity high

- Source: `crates/hya-provider/src/openai.rs:86 (also openai/responses.rs:192, anthropic.rs:68)`
- Evidence: docs/architecture/providers.md documents Google media encoding (line 92-93) but never states that the other protocols reject media. docs/compat-parity.md:89 only says Google encodes media parts. Grep for `does not support media type` across in-scope docs: no hits. A user attaching an image on an Anthropic or OpenAI route gets `Incompatible("... does not support media type ...")` with no doc explaining why.
- Write: In the OpenAI-Compatible and Anthropic protocol sections (and the new Responses section), add an explicit line: any `Part::Media` in the message history causes the encoder to fail the turn with `ProviderError::Incompatible("<protocol> does not support media type <mime>")`. Only the Google/Gemini route encodes media (as `inlineData`). Also add a short `## Image or file attachment fails with "does not support media type"` entry to docs/troubleshooting.md telling the user to switch the session to a `kind: google` route, since v2 prompt file attachments are replayed to providers as canonical media parts.

**3. HttpProvider default Capabilities (all routes report usage_reporting/streaming_tool_calls/parallel_tool_calls/reasoning_request = true, max_context = 200_000); Capabilities field list; DevProvider capabilities** — `contradicted` · severity high

- Source: `crates/hya-provider/src/http.rs:172, crates/hya-provider/src/lib.rs:63, crates/hya-provider/src/dev.rs:51`
- Evidence: docs/architecture/storage.md:110-111 states "Provider usage reporting is represented in the data model, but live HTTP routes currently declare `usage_reporting: false`." The code at http.rs:172 sets `usage_reporting: true` for every HTTP route. Separately, docs/architecture/providers.md:15 lists `Capabilities` as "Route features such as streaming tool call support" without naming the seven fields, and no doc mentions the 200_000 default or that there is no per-model capability table.
- Write: Add a `### Capabilities` subsection listing all seven fields from lib.rs:63 — `streaming_tool_calls`, `parallel_tool_calls`, `usage_reporting`, `json_output`, `reasoning_stream`, `reasoning_request`, `max_context` — and state the HTTP default (http.rs:172): every configured HTTP route, regardless of kind or model, reports streaming_tool_calls / parallel_tool_calls / usage_reporting / reasoning_request = true, json_output = reasoning_stream = false, and max_context = 200_000. Say explicitly that there is NO per-model capability table, so the context window surfaced by `GET /api/model` is the fixed 200k default and not the model's real limit. Add that `DevProvider` claims the same set minus `reasoning_request`, which is why it serves any model ref (dev.rs:51). SEPARATELY: fix docs/architecture/storage.md:110-111 — delete the `usage_reporting: false` claim and replace it with a note that live HTTP routes declare `usage_reporting: true` and that per-protocol usage frames feed `record_usage`.

**4. Anthropic max_tokens defaulting to 4096 and the thinking-budget bump** — `undocumented` · severity medium

- Source: `crates/hya-provider/src/anthropic.rs:36`
- Evidence: docs/architecture/providers.md:71-84 documents the Anthropic encoder's system/tools/tool_use pairing and stop-reason mapping but says nothing about `max_tokens`. Grep for `max_tokens` and `4096` across in-scope docs: no hits.
- Write: In the `## Anthropic Protocol` section add: the encoder always emits `{model, messages, stream:true, max_tokens}`; when `CompletionRequest.max_output_tokens` is absent, `max_tokens` defaults to 4096, which silently caps output length on Anthropic routes. When a thinking budget is set (from the reasoning effort), `max_tokens` is raised to budget + 4096 if the configured value would not already exceed the budget. Also note that `System` messages appearing in the message history are dropped — the system prompt only reaches Anthropic through the top-level `system` field (anthropic.rs:13).

**5. ProviderRouter strips `req.reasoning` when the resolved route lacks `reasoning_request`** — `undocumented` · severity medium

- Source: `crates/hya-provider/src/router.rs:75`
- Evidence: docs/architecture/providers.md `## Provider Router` (lines 21-27) only describes model resolution and `UnknownModel`. No doc mentions that the router clears the reasoning field before dispatch.
- Write: In `## Provider Router`, add: before dispatching, the router clears `CompletionRequest.reasoning` when the resolved route's `capabilities()` do not advertise `reasoning_request`. This means a configured `reasoning.default` is silently dropped (not an error) on a route that does not support reasoning requests, so no reasoning parameter is ever sent to an upstream that would reject it.

**6. Google inline-media constraints: 13-entry MIME allowlist, 28 MiB encoded / 20 MiB decoded size caps, base64 and `data:` URL validation** — `thin` · severity medium

- Source: `crates/hya-provider/src/google.rs:13, :28, :91`
- Evidence: docs/architecture/providers.md:93 says only "image, video, and audio data are passed as validated base64 `inlineData`". Grep for the MIME list, `28`, `20 MiB`, `data:` across in-scope docs: no hits. docs/compat-parity.md:89 vaguely says "Compat's current image, audio, and video MIME set".
- Write: In `## Google Protocol`, replace the one-line media claim with the real contract from google.rs. Enumerate the 13 accepted MIME types from the allowlist at google.rs:13 (read them off the constant) and state that any other MIME is rejected with `ProviderError::Incompatible`. Document the size caps at google.rs:28: inline media is capped at 28 MiB encoded and 20 MiB decoded. Document the accepted payload forms at google.rs:91: either a raw base64 payload, or a `data:<mime>;base64,<payload>` URL whose declared mime in the header must match the part's declared MIME type; non-canonical base64 is rejected.

**7. configured_identity_v1 routing fingerprint (Provider trait method, HTTP identity contents, secret-free auth identity, router fail-closed aggregation)** — `undocumented` · severity medium

- Source: `crates/hya-provider/src/lib.rs:347, crates/hya-provider/src/http.rs:367, :420, crates/hya-provider/src/router.rs:26`
- Evidence: Rustdoc exists on the trait method only ("Return deterministic configured routing identity, excluding secrets"). Grep across all in-scope docs for `configured_identity`, `identity`, `fingerprint`: docs/architecture/event-model.md:35 lists "assistant-turn runtime binding identity" as an event group, but nothing explains what the provider identity contains or that it fails closed.
- Write: Add a `## Configured Identity` section. Explain the purpose: a deterministic, secret-free fingerprint of a route's configuration used to detect that a TurnBinding's provider config changed. `Provider::configured_identity_v1` returns `Option<Vec<u8>>` and providers without a complete identity fail closed by returning `None` (lib.rs:347). For HTTP routes the fingerprint covers (http.rs:367): the tag `hya.provider.http.configured.v1`, the crate version, provider id, kind, endpoint, google base, alias rules, the sorted model set, per-model reasoning variants, capabilities, auth shape, and whether a bearer resolver is installed. Emphasise what is deliberately excluded (http.rs:420): only the auth style tag, a boolean "secret is non-empty" flag, and non-secret fields (codex account id, grok client version/identifier, anthropic version) enter the fingerprint — never the token itself. Finally document `ProviderRouter::configured_identities_v1` (router.rs:26): it returns per-provider identities in insertion order, or `None` if ANY provider returns `None` or an empty identity — so putting a `FakeProvider` in the router fails the whole set closed.

**8. FakeProvider and the FakeStep script vocabulary** — `thin` · severity medium

- Source: `crates/hya-provider/src/fake.rs:24, :13`
- Evidence: docs/architecture/providers.md:105-106 says only "`FakeProvider` replays scripted `FakeStep`s and is used by tests". No doc lists the FakeStep variants or the exhausted-turn behavior, so a test author cannot write a script from the docs. docs/testing/README.md does not mention it at all.
- Write: Expand the FakeProvider bullet in `## Fake and Dev Providers` into a short subsection. FakeProvider has id `fake` and replays one scripted step list per assistant turn. List the `FakeStep` vocabulary (fake.rs:13): `Text(String)`, `Reasoning(String)`, `ToolCall{name, input}`, `Usage(TokenUsage)`, `Finish(FinishReason)`. State the termination contract: once the script's turns are exhausted, every further turn emits a bare `Finish(Stop)` so agent loops terminate instead of hanging. Add a two-line construction example. Note (see the identity section) that a FakeProvider in the router makes `configured_identities_v1` return `None` for the whole set.

**9. Provider trait method contract (id, capabilities, configured_identity_v1, catalog, stream, compact_responses)** — `thin` · severity medium

- Source: `crates/hya-provider/src/lib.rs:341`
- Evidence: docs/architecture/providers.md:12 gives a one-line purpose ("A route that can stream a `CompletionRequest` for supported models") but no method list or signatures. Rustdoc exists only on `configured_identity_v1` and `compact_responses`; `id`, `capabilities`, `catalog` and `stream` carry none.
- Write: Under `## Core Traits`, add the `Provider` method contract as a list: `id() -> &str` (the configured provider id, also the auth filename and the `providerID` in model refs); `capabilities(&ModelRef) -> Option<Capabilities>` (returning `Some` is what claims the model — this is how the router resolves, first match wins); `configured_identity_v1() -> Option<Vec<u8>>` (default `None`, fails closed); `catalog() -> Vec<ProviderModel>` (default empty); required `async stream(req, session, message) -> Result<EventStream, ProviderError>`; and optional `async compact_responses(...) -> Result<Option<CompactedWindow>, ProviderError>` (default `Ok(None)`). Mark which have defaults so an implementor knows the minimum surface. Also add the `Protocol` (`encode(&CompletionRequest) -> serde_json::Value`, `decoder(session, message)`) and `Decoder` (`push(&str)`, `finish()`, each returning canonical `Event` batches) signatures.

**10. ProviderError variants (Json, Http, UnknownModel, Incompatible, Decode, AuthExpired)** — `thin` · severity medium

- Source: `crates/hya-provider/src/lib.rs:46`
- Evidence: Only two variants leak into the docs: docs/configuration.md:342 mentions the `unknown provider for model` message and docs/troubleshooting.md:31 has a section for it. `Json`, `Decode`, `Incompatible` and `AuthExpired` are never listed anywhere as an error taxonomy.
- Write: Add an `### Errors` subsection listing the `ProviderError` variants and their display strings from lib.rs:46: `Json` (`json: ...`, serde failure while encoding/decoding), `Http(String)` (`http: ...`, non-2xx status or in-stream error frame), `UnknownModel(String)` (`unknown provider for model: ...`, no route claims the ref), `Incompatible(String)` (`incompatible route: ...`, preflight or an unsupported part such as media), `Decode(String)` (`decode: ...`, malformed or truncated stream), and `AuthExpired{provider, hint}` (`auth expired for provider '<p>': <hint>`, produced only by the OAuth bearer resolver wiring in hya-app). For each, say which troubleshooting entry covers it.

**11. reqwest client policy: redirects disabled, 10s connect timeout, no read timeout** — `thin` · severity medium

- Source: `crates/hya-provider/src/http.rs:119`
- Evidence: docs/architecture/providers.md:43-44 says only "redirects are disabled" and "connect timeout is set" — the 10s value and the deliberate absence of a read timeout are not stated anywhere.
- Write: In the HTTP Provider security bullets, give the concrete values and the rationale from http.rs:119: redirects are disabled so an `x-api-key` cannot be forwarded cross-origin on a 3xx; the connect timeout is 10 seconds; and there is deliberately NO read/total timeout, so a long streaming completion is never aborted mid-stream by the client. Note the consequence: a hung upstream that has already accepted the connection will not time out on hya's side.

**12. Per-protocol usage extraction field mapping (OpenAI chat, Anthropic, Google)** — `undocumented` · severity medium

- Source: `crates/hya-provider/src/openai/decoder.rs:197 (also anthropic/decoder.rs:261, google.rs:338, openai/response_decoder.rs:295)`
- Evidence: docs/architecture/storage.md:95-108 lists the token-ledger columns but not which upstream fields fill them. Grep for `cached_tokens`, `cache_read_input_tokens`, `thoughtsTokenCount`, `reasoning_tokens` in-scope: no hits.
- Write: Add a `## Usage Reporting` section with one row per protocol showing the source field for each canonical TokenUsage slot. OpenAI chat (decoder.rs:197): `prompt_tokens`, `completion_tokens`, `completion_tokens_details.reasoning_tokens`, `prompt_tokens_details.cached_tokens`, `prompt_tokens_details.cache_creation_tokens`. OpenAI Responses (response_decoder.rs:295): `/response/usage` `input_tokens`, `output_tokens`, `output_tokens_details.reasoning_tokens`, `input_tokens_details.cached_tokens`; cache_write is always 0. Anthropic (anthropic/decoder.rs:261): `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`; reasoning tokens are always 0. Google (google.rs:338): `usageMetadata.promptTokenCount`, `candidatesTokenCount`, `thoughtsTokenCount` (as reasoning), `cachedContentTokenCount`; cache_write is always 0. Call out the always-zero fields so nobody reads a 0 as a real measurement.

**13. Per-request extra headers (`CompletionRequest.headers`) merge and sensitivity semantics** — `thin` · severity medium

- Source: `crates/hya-provider/src/http.rs:314`
- Evidence: docs/architecture/providers.md:16 mentions "request headers" as a CompletionRequest field but does not say they are merged over auth headers, marked sensitive, or that a malformed name/value fails the call.
- Write: In the HTTP Provider section, document that `CompletionRequest.headers` are merged OVER the route's auth headers on every call — a plugin-supplied header therefore wins over the auth header of the same name — that every value is marked sensitive on the `HeaderValue` so reqwest/tracing will not log it, and that an invalid header name or value fails the call with `ProviderError::Http` rather than being silently dropped (http.rs:314).

**14. ProviderRouter ordering semantics and catalog aggregation** — `thin` · severity low

- Source: `crates/hya-provider/src/router.rs:10, :53`
- Evidence: docs/architecture/providers.md:21-27 says the router "keeps an ordered list of providers" and asks each in turn, but does not state that insertion order decides ties, nor describe the catalog sort/dedup at router.rs:53.
- Write: In `## Provider Router`, add: resolution returns the FIRST provider whose `capabilities()` returns `Some`, so when two configured routes both serve the same bare model id, the earlier one in insertion order wins — this is the tie-break users hit when they configure two OpenAI-compatible gateways with overlapping model lists. Also document `ProviderRouter::catalog` (router.rs:53): it flattens every provider's catalog, sorts by `(provider_id, model_id)`, and dedups identical provider/model pairs, which is the ordering seen by `hya-backend models` and `GET /api/model`.

**15. Anthropic API version pin `anthropic-version: 2023-06-01`** — `thin` · severity low

- Source: `crates/hya-provider/src/http.rs:147`
- Evidence: docs/architecture/providers.md:46 names the `anthropic-version` header but never gives the pinned value or says it is not configurable. Grep for `2023-06-01` across all in-scope docs: no hits.
- Write: In the HTTP Provider security bullets, state that Anthropic routes hardcode `anthropic-version: 2023-06-01` and that the value is NOT configurable through `config.yaml` — changing it requires a code change at crates/hya-provider/src/http.rs:147.

**16. OpenAI chat finish-reason mapping and null tool-input normalization** — `thin` · severity low

- Source: `crates/hya-provider/src/openai/decoder.rs:79, crates/hya-provider/src/openai.rs:142`
- Evidence: docs/architecture/providers.md:65 says only "finish reasons map to hya `FinishReason`" — the Anthropic section gives an explicit mapping but the OpenAI one does not. The null tool-input normalization is not mentioned anywhere.
- Write: In `## OpenAI-Compatible Protocol`, replace the vague finish-reason bullet with the actual mapping (decoder.rs:79): `tool_calls` → ToolCalls, `length` → Length, `content_filter` → Error, and everything else — including an absent finish reason — → Stop. Also add an encoder note: a stored tool input that is JSON `null` is serialized as the string `"{}"` in `function.arguments` (openai.rs:142), because the wire format requires a JSON object string. Mention the decoder closes on `[DONE]` or plain stream end (decoder.rs:30).

**17. Google finish-reason mapping (tool call forces ToolCalls; SAFETY/RECITATION → Error)** — `thin` · severity low

- Source: `crates/hya-provider/src/google.rs:236`
- Evidence: docs/architecture/providers.md:98-99 says the decoder "maps streamed text, function calls, and finish reasons into hya's canonical event variants" without the table. Grep for `RECITATION`, `SAFETY` in-scope: no hits.
- Write: In `## Google Protocol`, add the finish-reason mapping (google.rs:236): if a function call was seen in the stream the finish reason is forced to `ToolCalls` regardless of what Gemini reported; otherwise `MAX_TOKENS` → Length, `SAFETY` and `RECITATION` → Error, everything else → Stop. Also note the decoder reads `candidates[0]` only, coalesces all text parts into a single text part, and closes on the first `finishReason` it sees (google.rs:200).

**18. HttpProvider::new construction contract** — `thin` · severity low

- Source: `crates/hya-provider/src/http.rs:109`
- Evidence: docs/architecture/providers.md:29-39 lists what an HttpProvider owns but not the constructor arguments or the base_url normalization. No rustdoc on `new`.
- Write: In `## HTTP Provider`, add one paragraph on construction: `HttpProvider::new(id, kind, base_url, api_key, models)` builds one route; the `ProviderKind` selects the protocol encoder/decoder, the endpoint path and the auth style, and a trailing `/` is trimmed off `base_url` before the endpoint is built — so `https://host/v1` and `https://host/v1/` behave identically (http.rs:109). Mention the builder methods that layer on top (`with_model_reasoning_variants`, `with_codex_session_auth`, `with_grok_session_auth`, `with_bearer_resolver`) and that the session-auth upgrades are no-ops for other kinds so callers can chain unconditionally.

**STALE 1.** The document claims: The HTTP provider section enumerates the auth/endpoint behavior for exactly three families — Anthropic (`x-api-key`), "OpenAI-compatible" (Bearer), and Google (`x-goog-api-key`) — and the protocol sections cover only OpenAI Chat Completions, Anthropic, and Google.

- Reality: There are six `ProviderKind`s (http.rs:32-38) and five auth styles (Bearer, CodexSession, GrokSession, Anthropic, Google). The Responses wire (`openai-response`, `openai-codex`, `grok-build`) — its encoder, its `output_index`-keyed decoder, its typed-terminal requirement, and its compact endpoint — has no section in this file at all, so the page reads as a complete protocol list while omitting half the shipped routes.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: The `Provider` trait row describes it as "A route that can stream a `CompletionRequest` for supported models."

- Reality: The trait has since grown `configured_identity_v1` (routing fingerprint, fails closed) and `compact_responses` (native context compaction) alongside `id`/`capabilities`/`catalog`/`stream` (crates/hya-provider/src/lib.rs:341). The one-line description no longer covers the trait's actual surface, so an implementor reading only this table would miss two methods with real defaults and real consequences.
- Action: correct or delete. Do not merely supplement.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 18 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
