# 0.41.0

## Every bundle has one `config.yml`

- Each bundle now reads its configuration from one file. For bundles installed for the user, and for the builtin first-party bundles, the file is `<hya config dir>/bundles/<percent-encoded-bundle-id>/config.yml`. The `<hya config dir>` is the directory that holds the active `config.yaml`. For example, `hya/plan-impl-review` reads `~/.config/hya/bundles/hya%2Fplan-impl-review/config.yml`. A project bundle (`hya bundle install --project`) reads `config.yml` in its own `.hya/bundles/<dir>/` source directory.
- **Breaking:** Bundle Agent model defaults (`agents.<agent-id>.model`) now live in this file. Hya no longer reads the old `<hya config dir>/agents/<encoded-bundle-id>/config.yml`: move each file to `bundles/<encoded-bundle-id>/config.yml`. Saves still change only the model leaf and keep every other key, so a bundle can store its own settings in the same file.
- `extensions.process` providers, bundle MCP stdio servers, and agent sidecars all receive the absolute `HYA_BUNDLE_CONFIG_DIR` and `HYA_BUNDLE_CONFIG_FILE` paths. The file does not have to exist. Process argv, MCP argv, and MCP `env` values also expand `${BUNDLE_CONFIG_DIR}` and `${BUNDLE_CONFIG_FILE}`. Bundle MCP servers get the inherited `PATH` and `HYA_BUNDLE_ROOT` as well. A key declared in the MCP `env` map overrides the config variables and `PATH`.
- If you edit the `config.yml` of a bundle that runs a process or MCP server, that bundle's providers restart at the next root binding, the same as when the bundle itself changes.
- A project bundle's `config.yml` is not bundle content. It doesn't enter the bundle's sources, digest, or project fingerprint. A reinstall or upgrade keeps the existing file, and an incoming package never writes one.

## Plugin hooks reach bundle agents, and `chat.params` knows the request chain

- An installed Plugin's hooks now run for bundle-defined agents too, not only for built-in agents. A bundle agent's chain is every installed Plugin's hooks (ascending bundle id), then its own bundle's hooks filtered by `hook_refs`, then its activation sidecar's hooks. A Plugin's `chat.params`, `tool.execute.before` veto, and `permission.ask` answer therefore apply to every session. Subagent and resident members keep their sidecar hooks when Plugin hooks join their turn.
- `hook/chat.params` params gain two optional fields: `root_session`, the root of the session's spawn tree (equal to `session` for a root), and `agent`, the session's stable agent id. A plugin can use them to keep one decision per request chain. Plugins that ignore unknown fields keep working, and the hook's outcome is unchanged. The Bun adapter passes both fields to `chat.params` handlers.

## Plugins can choose the fallback model when a provider fails before streaming

- New hook `model.fallback` (`hook/model.fallback`, posture Open, always fail-open). A provider can fail before any stream exists. When the configured `categories:` chain can no longer advance, the engine asks this hook for the next model. The params are `session`, `root_session`, `agent`, `message`, the failed `model`, `error` `{ class, message }`, `attempt`, and `tried`. The error class is one of `retryable`, `unknown_model`, `auth`, `invalid_request`, or `other`. The hook answers `{ "outcome": "retry", "model": "provider/model" }` or `{ "outcome": "give_up" }`, and the first `retry` wins.
- Safety limits: a model already tried in the round is refused, a round makes at most 8 attempts, and the hook is never called once a stream exists. Workflow-routed turns don't call the hook. No new events are recorded; each switch logs a warning.
- Process-backed bundles (`extensions.process`) and configured plugins can declare `model.fallback`. The Bun adapter registers and dispatches it: a handler returns `{ outcome: "retry", model }`, a bare model string, or `{ outcome: "give_up" }`.

## Optional `hya-extra/*` bundles

- New `bundles/extra/` ships four optional bundles. None is installed by default: package one with `cargo run -p xtask -- package-bundle bundles/extra/<name> <name>.hyabundle`, then run `hya bundle install`. Each also serves as a coverage fixture, checked by `crates/hya-bundle/tests/extra_bundles.rs` and e2e rows T2.26–T2.28. See [hya-extra bundles](docs/extra-bundles.md).
- `hya-extra/zvec-grep` (Plugin): gives agents [zvec-grep](https://github.com/zvec-ai/zvec-grep) semantic workspace search. It runs `zg --server --stdio` as a bundle MCP server and ships a usage skill. It needs `npm install -g @zvec/zvec-grep`.
- `hya-extra/scout` (AgentSetBundle): the `scout` subagent, a cheap-model retrieval scout (`model_policy.category: quick`). It uses its own zvec-grep MCP server plus `read`/`grep`/`glob`, and returns answers with `path:line` evidence.
- `hya-extra/jev-model-router` (Plugin, Bun process, `chat.params`): asks TypeSafe's Jev model how hard a request is, then rewrites `request.model` to the matching configured tier. The decision sticks to the request chain (`root_session`) so prompt caches stay warm. It falls back to `default_tier` when Jev fails or is unsure. Configure it in its bundle `config.yml`.
- `hya-extra/model-fallback` (Plugin, Bun process, `model.fallback`): configured per-model fallback chains, for example `chains: { provider/a: [provider/b] }`. It retries the next model when a provider fails before streaming, limited by an error-class filter and `max_attempts`.
