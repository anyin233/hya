# Reasoning Effort Matrix Evidence

## Scope

This record captures the configuration contract and live probe evidence needed
to update the installed hya `0.33.14` user configuration. It contains no
credentials or response content.

## Released Configuration Contract

- `docs/configuration.md` documents `kind: openai-response` as the
  `/responses` route.
- A detailed model entry accepts:

  ```yaml
  - id: model-id
    reasoning:
      default: high
      variants: [low, medium, high]
  ```

- Accepted labels are `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, and
  `max`.
- A non-`none` default must appear in that model's variants.
- Omitting model variants inherits the provider-kind list, which would
  incorrectly advertise Responses `max` for GPT 5.4 and GPT 5.5.
- Anthropic routes use the released provider list `low`, `medium`, `high`,
  `max`.

## Configured Models And Target Matrix

| Provider | Model | Variants | Default |
| --- | --- | --- | --- |
| `12th-anth` | `claude-opus-4-8` | `low, medium, high, max` | `max` |
| `12th-anth` | `claude-opus-4-7` | `low, medium, high, max` | `max` |
| `12th-anth` | `claude-sonnet-4-6` | `low, medium, high, max` | `max` |
| `12th-oai` | `gpt-5.6-sol` | `none, minimal, low, medium, high, xhigh, max` | `max` |
| `12th-oai` | `gpt-5.5` | `minimal, low, medium, high, xhigh` | `xhigh` |
| `12th-oai` | `gpt-5.4` | `minimal, low, medium, high, xhigh` | `xhigh` |
| `12th-cn` | `glm-5.2` | `low, medium, high, max` | `max` |
| `12th-cn` | `kimi-for-coding` | `low, medium, high, max` | `max` |

`12th-oai` must change from `kind: openai` to `kind: openai-response`. Model
and provider order stays unchanged.

## Live Boundary Evidence

Fresh probes against `https://api.12th.day/v1/responses` on 2026-07-21
returned HTTP 200, `status: completed`, and no API error for:

- `gpt-5.4` with `xhigh`
- `gpt-5.5` with `xhigh`
- `gpt-5.6-sol` with `max`

The archived `07-21-openai-responses-reasoning-effort` task also records HTTP
200 completed responses for all seven configured GPT 5.6 effort labels.

## Decision

Use explicit variants and defaults for every configured model. This is more
verbose than inheriting provider lists, but it keeps the requested maximum and
allowed choices visible and prevents GPT 5.4/5.5 from inheriting unsupported
`max`.
