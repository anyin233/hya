# Progress

## Done
- `models.yml.cache` read/write beside config.yaml
- `config::load` prefers cache for empty-models providers (no HTTP on warm cache)
- `refresh_pending_catalogs` + engine `publish_provider_catalog`
- serve/TUI backend spawn background refresh → SSE `catalog.updated`
- sync.tsx reapplies `/config/providers` on `catalog.updated`
- quality guidelines + CHANGELOG 0.36.25

## Verify
- `cargo test -p hya-app --test provider_models_cache`
- `bun run typecheck` in packages/hya-tui-ts
