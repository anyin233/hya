# 0.43.22

## Fixes

- **`/model` now switches a pinned agent's session model.** When `config.yaml` pinned the session agent's model (`agents.<id>.model`), or a bundle policy did, `UpdateSession {model}` changed the model the session showed, but requests kept going to the pinned model. The switch is now also recorded as the session tree's temporary choice for that agent, which outranks the pin. New sessions still start on the pinned model.
- **Models of providers added at runtime can become an agent's default.** The agent-model control checked models against the startup provider routes. Picking a model from a provider added or refreshed after startup (for example with the Provider View's wizard) failed with `unavailable`, and `/model` reported an error. It now checks the engine's live routes.

## Features

- **One `/agent` view for every agent.** `/agent` now opens a full-screen Agents view instead of a picker. It has three sections, each under a titled divider: primary agents, subagents, and system agents (`compaction`, `summary`, `title`, which were hidden before). Enter selects a primary agent for the session. `m` opens the agent's model list and `t` its effort list. `c` clears a remembered model, and `r`, `/`, and Esc work as before. `/agent <name>` still switches directly. The `/agent-models` command and its view are removed; this view replaces them.
- **Change a pinned agent's model from the TUI.** Picking a model with `m` for an agent pinned in `config.yaml` (`agents.<id>.model`), or in a bundle's `config.yml`, writes that file and takes effect without a restart. New rpc `AgentModels.SaveAgentModelConfiguration` (`PUT /v1/agent-models/{agent_id}/configuration`, body `{directory?, session?, model?}`; omitting `model` clears the entry). `AgentModelState` gains `configurationPath`.

## Release and installation

- **One-command install from a release.** Run `curl -fsSL https://hya.ed-aisys.com/install.sh | sh`. That URL redirects to the newest release's `hya-install.sh`. It installs the newest release into `~/.local` (`--prefix`, `--version`, `--force`; `HYA_REPO`, `HYA_RELEASES_URL`, `HYA_VERSION`, `HYA_INSTALL_DIR`, `HYA_TARGET`). The installer checks the archive against the release's `SHA256SUMS`, swaps files in by rename, and rolls back if the installed `hya --version` does not match. The script is `scripts/hya-install.sh` and can be hosted anywhere. See `docs/install.md`.
- **Bare `hya update` moves an installed hya to the latest release.** It runs the same installer, compiled into the binary, for the running hya's prefix. `hya update --version X`, `--force`, and `--prefix DIR` are also accepted. The signed-release subcommands (`hya update status|apply|…`) are unchanged, and you cannot combine them with these options.
- **Release CI publishes a complete package per platform.** The tag-triggered release adds `x86_64-apple-darwin` (built on `macos-15-intel`), so it now builds four targets. Each archive also ships the pinned Bun 1.4.2 at `lib/hya/bin/bun`, and `THIRD_PARTY_NOTICES`. hya prefers that Bun over the one on `PATH` (`$BUN` still wins), so a release install needs no separate Bun. The release also publishes `hya-install.sh`. Every build job installs its archive with the installer and runs `hya update` before upload. Tags with a `-` suffix are published as prereleases, so `latest` skips them.
