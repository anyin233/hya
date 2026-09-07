# Design: Highest Reasoning Effort Defaults

## Boundary

Change only `/home/yanweiye/.config/hya/config.yaml`. The released hya `0.33.14`
configuration contract already represents the requested behavior, so no source,
dependency, version, changelog, or release changes are needed.

The edit preserves:

- `default_model` and `default_agent`
- provider IDs, base URLs, and ordering
- model IDs and ordering
- MCP and permission configuration
- authentication files and values

## Configuration Shape

Each shorthand model string becomes a detailed entry with `id`,
`reasoning.default`, and `reasoning.variants`. `12th-oai.kind` changes from
`openai` to `openai-response`; no other provider field changes.

| Models | Variants | Default |
| --- | --- | --- |
| Three `12th-anth` models | `[low, medium, high, max]` | `max` |
| `12th-oai/gpt-5.6-sol` | `[none, minimal, low, medium, high, xhigh, max]` | `max` |
| `12th-oai/gpt-5.5`, `gpt-5.4` | `[minimal, low, medium, high, xhigh]` | `xhigh` |
| Two `12th-cn` models | `[low, medium, high, max]` | `max` |

## Runtime Contract

At startup, hya parses each configured label into `ReasoningEffort`, verifies a
non-`none` default appears in its model's variants, and publishes those variants
in the provider catalog. The selected default reaches the initial agent model
configuration. `openai-response` routes requests to `/responses` and preserves
all configured effort labels on the wire.

## Verification Design

1. Capture the current checksum, metadata, and eight-model catalog in a private
   temporary backup directory.
2. Run one structured Ruby/YAML target assertion before editing. It must fail
   for the intended missing state: shorthand models and `kind: openai`.
3. Apply one narrow text patch.
4. Rerun the identical assertion and require success.
5. Compare parsed before/after data against an expected object derived from the
   baseline with only the approved mutations.
6. Run installed `hya-backend models` and require the same eight qualified IDs.
7. Inspect the exact backup-to-current diff before removing the backup.

The live endpoint boundary has already been checked for all three GPT maxima;
implementation does not repeat paid probes.

## Failure And Rollback

Any unexpected pre-edit drift stops execution. Any post-edit assertion,
semantic comparison, or runtime smoke failure restores the metadata-preserving
backup, verifies its checksum, and reruns the original model-catalog smoke.
There is no fallback to Chat or a lower effort without user approval.

## Trade-off Resolution

One planner proposed inheriting provider variants for Anthropic models and GPT
5.6 to reduce YAML. The merged design instead lists every variant explicitly:
the user requested visible per-model maxima, the PRD requires auditable model
metadata, and GPT 5.4/5.5 already need explicit overrides. No additional helper,
schema, or repository code is justified.
