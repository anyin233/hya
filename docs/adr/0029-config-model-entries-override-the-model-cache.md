# Config model entries override the model cache

Status: Accepted, 2026-09-29.

A provider's models come from two places. The **model cache**
(`model_cache.db`) holds what the provider's `GET <base>/models` returned:
ids plus whatever metadata the endpoint reports. The provider's **`models:`
entries** in `config.yaml` hold what the user declared. `hya provider add`,
the TUI Provider View, `hya models --refresh`, and startup discovery all fill
the cache. The user and the Provider View's model editor write `models:`.
Both describe the same model ids, so hya needs one rule for which one wins.

## Decision

**Merge per model id, field by field, and let config win.**

| Model id is in | Effective model | Source |
| --- | --- | --- |
| the cache only | cached values | `remote` |
| `models:` only | the entry's values (protocol defaults for unset fields) | `config` |
| both | each field the entry sets wins; each unset field falls back to the cached value | `override` |

- The overridable fields are `name`, `limit.context`, `limit.output`,
  `reasoning` (menu and default), `modalities.input`, and `kind` (the
  per-model protocol). A cached value never contradicts a configured one: if
  the merged `limit.output` would exceed `limit.context`, the cached side of
  that pair is dropped.
- An entry is never deleted or rewritten by a fetch. A refresh replaces only
  the provider's cache rows, and re-adding a provider (`hya provider add`,
  `PUT /v1/providers/{id}`) replaces only its `kind` and `base_url`. A
  model the endpoint stops listing stays available as `config` when an entry
  names it.
- Fetched models are **not** copied into `config.yaml`. `models:` stays the
  user's list of deliberate declarations and overrides, and the cache
  follows the endpoint.
- The source is visible everywhere a model is listed: v1
  `ModelSummary.source`, `hya models --verbose`, and `hya provider list`
  (`(config override)` / `(config only)`).

Implementation: `merge_provider_models` and `override_model` in
`crates/hya-app/src/config.rs`. The test
`effective_models_merge_cache_and_config_per_id_with_field_level_override`
pins the rule.

## Consequences

- A user fixes wrong or missing endpoint metadata by adding a
  `- id: <model>` entry with only the fields to change. Everything else
  keeps following the endpoint.
- A model the endpoint does not list, such as one that is served but not
  advertised, can be used by declaring it. It shows as `config only`.
- A removed entry stops overriding at once. The next rebuild uses the cached
  row alone.
- The rule is per field, so an entry cannot "blank out" a cached value it
  does not set. To hide a cached display name or limit, set it explicitly.
