# Configure highest reasoning effort defaults

## Goal

Configure the installed hya `0.33.14` user configuration so every configured
model uses its highest supported reasoning effort by default, with the allowed
effort choices visible in the configuration.

## Background

- The installed `hya` and `hya-backend` binaries report version `0.33.14`.
- The user configuration is stored at `~/.config/hya/config.yaml`.
- The current config declares eight shorthand models across `12th-anth`,
  `12th-oai`, and `12th-cn`.
- hya supports detailed per-model reasoning defaults and allowed variants.

## Requirements

- Preserve unrelated provider, authentication, MCP, agent, and model settings.
- Use only configuration fields accepted by hya `0.33.14`.
- Expand all eight model entries with explicit `id`, `reasoning.default`, and
  `reasoning.variants` fields while preserving their order.
- Use `[low, medium, high, max]` with default `max` for all five models routed
  through the two Anthropic providers.
- Change `12th-oai` to `kind: openai-response`; all three configured GPT models
  must use the Responses route.
- Use `[minimal, low, medium, high, xhigh]` with default `xhigh` for `gpt-5.4`
  and `gpt-5.5`.
- Use `[none, minimal, low, medium, high, xhigh, max]` with default `max` for
  `gpt-5.6-sol`.
- Avoid repository code changes unless the released configuration contract
  cannot express the requested defaults.

## Acceptance Criteria

- [x] The resulting user configuration loads successfully in installed hya
  `0.33.14`.
- [x] Every configured reasoning-capable model defaults to its highest listed
  effort variant.
- [x] `12th-oai` uses `/responses` through `kind: openai-response`.
- [x] All eight model IDs and their order are unchanged.
- [x] Existing unrelated user configuration is unchanged.
- [x] The same structured target assertion fails before the edit for the
  intended missing state and passes afterward.
- [x] Installed `hya-backend models` loads the config and returns the same eight
  qualified model IDs.

## Out Of Scope

- Repository source, dependency, version, changelog, or release changes.
- Additional live endpoint probes after the three GPT maxima have passed.
- Authentication file or token changes.

## Technical Notes

- User selected Responses for all three `12th-oai` models. Minimal live probes
  returned HTTP 200 and `status: completed` for `gpt-5.4:xhigh`,
  `gpt-5.5:xhigh`, and `gpt-5.6-sol:max` on 2026-07-21.
- Omitting variants would inherit the provider-wide list and wrongly expose
  `max` for GPT 5.4/5.5, so their lists must be explicit.
