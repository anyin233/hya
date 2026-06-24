# OpenCode reasoning / variant schema (research)

Source: OpenCode (sst/opencode, repo mirror `anomalyco/opencode`) dev HEAD
`1ef0fd5d0105cc43a41548d6373231d49f8408ef`. Gathered via librarian on 2026-06-24.

## 1. Canonical reasoning level vocabulary

`ReasoningEfforts = ["none","minimal","low","medium","high","xhigh","max"]`
— `packages/llm/src/schema/ids.ts#L29-L34`.

Per-provider VALID subsets (this is the "correct variant per model" rule):

| Provider | Valid levels | Notes |
|---|---|---|
| OpenAI (wire) | none, minimal, low, medium, high, xhigh | **`max` is explicitly filtered OUT** (`openai-options.ts#L5-L8`). Per-model subset narrows further by model id/date (`transform.ts#L571-L587`). |
| Anthropic (budget) | high, max | high→16000, max→31999 (bounded by output limit). `transform.ts#L773-L785`, `#L925-L938`. |
| Anthropic (adaptive, opus 4.7+/sonnet 4.6+) | low, medium, high, xhigh, max | `thinking:{type:adaptive}` + `effort`. `transform.ts#L608-L619`, `#L898-L918`. |
| Google Gemini 2.5 | high, max | thinkingBudget: high=16000, max=24576 (32768 for 2.5-pro). `transform.ts#L647-L663`, `#L635-L638`. |
| Google Gemini 3 | low/medium/high (+minimal for flash) | thinkingLevel (not budget). `transform.ts#L626-L633`. |

## 2. Provider request wire mapping

- **Anthropic**: `thinking: { type: "enabled", budget_tokens: N }` (key is `budget_tokens` on the wire; OpenCode config uses `budgetTokens`). Adaptive: `thinking:{type:"adaptive"}` + `effort`. `anthropic-messages.ts#L491-L501`.
- **OpenAI Chat**: `reasoning_effort: <level>`. `openai-chat.ts#L331-L339`.
- **OpenAI Responses**: `reasoning: { effort, summary }`. `openai-responses.ts#L446-L464`.
- **Google**: `generationConfig.thinkingConfig: { includeThoughts, thinkingBudget | thinkingLevel }`. `gemini.ts#L92-L104`, `provider-options.ts#L82-L88`.

## 3. Config schema keys (opencode.json / opencode.jsonc)

- Provider-level generic options: `provider.<id>.options` (e.g. timeout). `config.mdx#L371-L385`.
- Per-model options: `provider.<id>.models.<id>.options` — object of provider request options. `models.mdx#L71-L100`.
- Per-model variants: `provider.<id>.models.<id>.variants.<name>` — named option bundles; `variants: Record<String, StructWithRest<{disabled?:bool}, [Record<String,Any>]>>`. `provider.ts#L61-L73`, `models.mdx#L110-L196`.
- Top-level `model`: plain `provider/model-id` — **NO `#variant` suffix** in config (`model.ts#L33-L39`). `provider/model/variant` is ONLY CLI/ACP selection syntax (`config-option.test.ts#L171-L188`).

Example (Anthropic per-model variants):
```jsonc
{ "provider": { "anthropic": { "models": { "claude-sonnet-4-5-20250929": {
  "options": { "thinking": { "type": "enabled", "budgetTokens": 16000 } },
  "variants": {
    "high": { "thinking": { "type": "enabled", "budgetTokens": 16000 } },
    "max":  { "thinking": { "type": "enabled", "budgetTokens": 31999 } }
  } } } } } }
```

## 4. Agent reasoning config

- Agent has a dedicated `variant` field (selects a model variant). `v1/config/agent.ts#L14-L18`.
- Agent unknown keys (e.g. `reasoningEffort`, `max_tokens`) are moved into `agent.options` (KNOWN_KEYS allowlist). `v1/config/agent.ts#L43-L66`, `config.test.ts#L655-L676`.
- Markdown agents: frontmatter parsed by gray-matter; `variant:` supported, unknown keys pass through to options. `config/markdown.ts#L3-L10`, `config/plugin/agent.ts#L116-L142`.
- Applying agent: `if item.variant -> agent.model.variant = variant`. `config/plugin/agent.ts#L77-L83`.

## 5. Implication for yaca

yaca today: `ReasoningEffort = {Low,Medium,High}` + per-provider budget fns; agent `variant`/`options` parsed but NEVER mapped to `AgentSpec.reasoning`. To reach parity:
- Extend the reasoning vocabulary + per-provider validity (OpenAI must never emit `max`).
- Read `provider.<id>.models.<id>.{options,variants}` from opencode.json (yaca-server opencode layer already loads opencode.json/jsonc via `global.config()`).
- Resolve agent `variant` -> selected variant bundle / effort -> provider request.
- Anthropic budgets to match: high=16000, max=31999 (bounded by output limit).
