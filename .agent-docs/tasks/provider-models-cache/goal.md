# Goal

Provider catalogs load from `$XDG_CONFIG_HOME/hya/models.yml.cache` first (async-safe, non-blocking), refresh discovery in the background, write the cache, then push the refreshed catalog into the TUI.

# Non-goals

- Frontend-owned catalog HTTP clients
- Mutating `config.yaml` model lists
- Changing configured (non-empty) `providers.*.models` authority
