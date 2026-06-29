# Per-model reasoning variants with OpenCode parity

## Goal

Make a model's reasoning **variant** (e.g. the `Sisyphus - ultraworker` agent's
`variant: max` on `claude-opus-4-8`) actually drive an extended-thinking request
to the provider, with **per-model validity**, mirroring OpenCode's config schema
and reading per-model reasoning settings from `opencode.json`.

Today `variant:` is cosmetic end-to-end: it is parsed and shown in the model
label but never becomes `AgentSpec.reasoning`, so no `thinking` request is sent
and no `type=reasoning` part is ever produced. (Verified 2026-06-24: opus
`variant: max` turns render visible text but emit zero reasoning parts.)

## Background (verified current state)

- `ReasoningEffort` is only `Low | Medium | High`; `parse("max") -> None`
  (`crates/hya-provider/src/lib.rs:74-116`).
- Agent frontmatter `variant` + `options` are parsed and stored in the opencode
  agent catalog (`agent_disk_sources.rs:22,29`, `agent_catalog.rs:17`) but never
  mapped to reasoning. `agent_with_model` hardcodes `reasoning: None`
  (`hya-app/src/runtime.rs:131`); `session_agent_with_guidance` copies only
  `system_prompt`+`name` (`hya-server/src/opencode/reference.rs:19-40`).
- Provider request sites already consume `req.reasoning` correctly when set:
  Anthropic `thinking{type,budget_tokens}` (`anthropic.rs:42-47`), OpenAI
  `reasoning_effort` (`openai.rs:59-60`), Google `thinkingConfig` (`google.rs:180-183`);
  router strips reasoning when `!caps.reasoning_request` (`router.rs:55-56`).
- hya-server opencode layer already loads `opencode.json`/`opencode.jsonc`
  (`global.rs:27`, `agent_sources.rs:133-136`).

See `research/opencode-reasoning-schema.md` for the OpenCode schema + citations.

## Scope (chosen: full OpenCode parity)

1. **Reasoning vocabulary + per-provider validity.** Support OpenCode's level set
   `none, minimal, low, medium, high, xhigh, max` with each provider's valid
   subset. OpenAI must NEVER emit `max` (invalid on the wire); Anthropic + Google
   2.5 support `max`.
2. **Per-model config from `opencode.json`.** Read
   `provider.<id>.models.<id>.options` and `provider.<id>.models.<id>.variants.<name>`
   as provider-option bundles, mirroring OpenCode.
3. **Agent variant resolution.** An agent's `variant:` (frontmatter / opencode.json
   agent) selects the model variant; unknown agent keys (e.g. `reasoningEffort:`)
   pass through as model options, per OpenCode.
4. **Provider request mapping.** Apply resolved options to the request:
   Anthropic `thinking` (high=16000, max=31999 — OpenCode's constant for ≥32k-output models),
   OpenAI `reasoning_effort` / `reasoning.effort`, Google
   `thinkingConfig.thinkingBudget|thinkingLevel`.
5. **Graceful invalid handling.** A variant a model does not support is ignored
   or clamped (never a hard error); e.g. an OpenAI model asked for `max` clamps
   to its highest supported level.

## Requirements

- R1: `claude-opus-4-8` under an agent with `variant: max` issues an Anthropic
  `thinking` request and the native `hya` TUI renders a `type=reasoning` part.
- R2: The reasoning level abstraction covers the OpenCode vocabulary with a
  per-provider validity gate; OpenAI never receives `max`.
- R3: Anthropic budgets align to OpenCode: `high`→16000, `max`→31999. 31999 is
  OpenCode's value for models with ≥32k output, which covers all configured
  models (opus/sonnet 4.x). A dynamic per-model output-limit bound (for any
  future <32k-output model) is a documented follow-up, not required here.
- R4: Per-model `options` and `variants` are read from `opencode.json` /
  `opencode.jsonc` and merged into the provider request, without a parallel reader
  (reuse the existing opencode config load path).
- R5: Agent `variant:` resolves to the request; pass-through agent options
  (e.g. `reasoningEffort:`) are honored.
- R6: Backward compatible. Existing `~/.config/hya/config.yaml` (no variants) and
  existing agents keep working; default reasoning is unchanged when no variant is
  set; sonnet (no variant) is unaffected.
- R7: hya-proto stays dependency-light; library crates keep `unwrap_used` /
  `expect_used` denied; event-sourced architecture preserved.

## Acceptance Criteria

- [ ] AC1: Driving `hya` (native) with the default `Sisyphus - ultraworker`
  (`opus-4-8`, `variant: max`) on a reasoning-forcing prompt produces a rendered
  reasoning/"Thinking" part in the TUI (verified in tmux, with event-log
  evidence of `type=reasoning` parts).
- [ ] AC2: A unit test proves the OpenAI mapping never emits `max` (clamps to the
  model's highest valid level) while Anthropic/Google 2.5 do emit `max`.
- [ ] AC3: A unit/integration test proves an `opencode.json`
  `provider.<id>.models.<id>.variants.max` bundle reaches the provider request
  body (Anthropic `thinking.budget_tokens` == 31999, the OpenCode constant for
  ≥32k-output models).
- [ ] AC4: An agent `variant:` (frontmatter) resolves to `AgentSpec.reasoning` /
  the request for the active model (covered by a test).
- [ ] AC5: Backward-compat test: a config with no variants and an agent with no
  `variant` sends no `thinking`/`reasoning_effort` (unchanged behavior).
- [ ] AC6: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` all pass.

## Out of scope

- New provider integrations or non-reasoning model options (temperature, top_p
  pass-through) beyond what is already supported.
- Changing the TUI reasoning rendering itself (only ensuring reasoning parts are
  produced so the existing renderer shows them).
- Migrating hya's native `config.yaml` to JSON; this task reads reasoning from
  the OpenCode config that the server layer already loads.

## Constraints

- Verification gate: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
- For OpenCode adapter changes (if any): `bun run typecheck && bun test` in
  `crates/hya-plugin-opencode/adapter`.
- No `as any` / `@ts-ignore` equivalents; no suppression of type errors.
- Smallest correct change; reuse existing planes/loaders over adding parallel ones.

## Open design questions (resolve in design.md)

- DQ1: Keep the simple `ReasoningEffort` enum (extended with the full level set +
  a provider-validity function) vs. adopt OpenCode's richer "variant = arbitrary
  provider-option bundle" model. Parity argues for option bundles; hya's current
  surface argues for the enum. Decide the boundary.
- DQ2: Where variant resolution lives (hya-core engine vs. hya-server opencode
  agent resolution vs. hya-provider router) and how the per-model `variants` map
  flows from config → request without breaking the in-process native path.
- DQ3: How `max` budget bounding by model output limit is sourced (model
  capability metadata availability in hya-provider).
