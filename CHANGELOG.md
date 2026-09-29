# 0.43.25

## Breaking changes

- **`hya auth` is removed, and `hya providers` is now an alias of `hya provider`.** Use `hya provider list` instead of `hya auth list`. It shows every provider with its key source, then saved keys for ids not in `config.yaml`. Use `hya provider logout <id>` instead of `hya auth logout <id>`: it deletes the saved key, keeps the declaration, and a running backend drops the key without a restart. `hya login` and `hya oauth` are unchanged.

## Features

- **`hya provider`: add, list, and remove model providers from the command line.** `hya provider add` asks for the base URL, the protocol (`openai-chat`, `openai-responses`, or `anthropic-messages`), and the API key (hidden on a terminal). It fetches the provider's model list with that key and shows it. Only then does it ask for a name (derived from the host, e.g. `api.12th.day` → `12th`) and save the provider: `providers.<id>` to `config.yaml`, the key to `auth/<id>.yaml`, and the models to the model cache. A key the endpoint rejects saves nothing unless you confirm. Every value can also be given as a flag (`--name`, `--base-url`, `--protocol`, `--api-key`, `-y`) for scripts. `hya provider list [--refresh]` shows each provider's protocol, base URL, key source, and models. A model that a `models:` entry overrides is marked `(config override)`, and one only config declares is marked `(config only)`. The rule, now recorded as ADR-0029: config wins field by field over the fetched metadata, and unset fields fall back to the cache. `hya provider remove <id> [-y]` deletes the provider, its saved key, and its cached models, and warns about `default_model` or agent models that still name it. A running backend picks up both changes without a restart. See docs/cli.md, "`hya provider`".

## Fixes

- **Source-checkout TUI startup is verified with the real OpenTUI renderer.** A render test catches a blank-screen failure before backend data arrives. Development setups that place `node_modules` on another disk now document the resolved path OpenTUI needs.
- **`hya serve start` no longer fails after a quick stop or restart.** A stopping server released the database lock before its runtime-owner lock, so a server started in that gap failed with `RUNTIME_OWNER_BUSY` and the start gave up. The server now releases the runtime owner first. A start that still meets a held owner exits 75 ("database in use"), so the starter waits and retries.
- **A prompt sent from `/status`, `/models`, `/todos`, `/workflows`, or `/api` shows its reply.** Those pages replaced the transcript until you opened a session, so the reply stayed out of sight. Sending a prompt or `!command` now returns to the transcript.
- **The WebUI address stays on the status bar at about 80 columns.** The `<model>:<effort>` segment pushed it off the line, and the sidebar that also shows it is hidden at that width. The WebUI segment now comes before the directory.
- **`/model <provider/model>` reports the model it switched to.** A session-list read that started before the switch could briefly roll the open session back and name the old model. Session rows older than the open session (by `lastSeq`) are now ignored.
- **Retrying a refused bundle install no longer fails as busy.** A refused install or uninstall now rolls its transaction back before it returns. Before, a `hya bundle install --overwrite` right after a refusal could hit `BundleRegistryBusy`.
- **`hya update` no longer reports its root as owned right after a previous step released it.** A child process forked by another thread briefly shares the updater lease's lock until it starts, so a lease released a moment earlier could still look held. Taking the lease now retries for up to 500 ms before it reports `LeaseUnavailable`.

The previous 0.43.24 notes are archived in `docs/changes/CHANGELOG_0.43.24.md`.
