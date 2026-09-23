# 0.41.0

## Every bundle has one `config.yml`

- Each bundle now reads its configuration from one file. For bundles installed for the user, and for the builtin first-party bundles, the file is `<hya config dir>/bundles/<percent-encoded-bundle-id>/config.yml`. The `<hya config dir>` is the directory that holds the active `config.yaml`. For example, `hya/plan-impl-review` reads `~/.config/hya/bundles/hya%2Fplan-impl-review/config.yml`. A project bundle (`hya bundle install --project`) reads `config.yml` in its own `.hya/bundles/<dir>/` source directory.
- **Breaking:** Bundle Agent model defaults (`agents.<agent-id>.model`) now live in this file. Hya no longer reads the old `<hya config dir>/agents/<encoded-bundle-id>/config.yml`: move each file to `bundles/<encoded-bundle-id>/config.yml`. Saves still change only the model leaf and keep every other key, so a bundle can store its own settings in the same file.
- `extensions.process` providers, bundle MCP stdio servers, and agent sidecars all receive the absolute `HYA_BUNDLE_CONFIG_DIR` and `HYA_BUNDLE_CONFIG_FILE` paths. The file does not have to exist. Process argv, MCP argv, and MCP `env` values also expand `${BUNDLE_CONFIG_DIR}` and `${BUNDLE_CONFIG_FILE}`. Bundle MCP servers get the inherited `PATH` and `HYA_BUNDLE_ROOT` as well. A key declared in the MCP `env` map overrides the config variables and `PATH`.
- If you edit the `config.yml` of a bundle that runs a process or MCP server, that bundle's providers restart at the next root binding, the same as when the bundle itself changes.
- A project bundle's `config.yml` is not bundle content. It doesn't enter the bundle's sources, digest, or project fingerprint. A reinstall or upgrade keeps the existing file, and an incoming package never writes one.
