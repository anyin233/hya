# 0.36.25

## Provider catalog cache

- Empty-`models` providers load model metadata from `$XDG_CONFIG_HOME/hya/models.yml.cache` on startup so cold listen does not wait on discovery HTTP.
- Cache rows store id plus `limit.context` / `limit.output`, reasoning default/variants, and tools — not bare id lists.
- Background discovery refreshes the cache, swaps the live engine catalog/router, and emits Compat SSE `catalog.updated`.
- The TUI Sync context re-fetches `/config/providers` on `catalog.updated` and reconciles the provider store after paint.
- Explicit `providers.*.models` in `config.yaml` remains authoritative; discovery never rewrites config.yaml.
