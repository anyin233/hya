# 0.43.22

## Fixes

- **`/model` now switches a pinned agent's session model.** When `config.yaml` pinned the session agent's model (`agents.<id>.model`), or a bundle policy did, `UpdateSession {model}` changed the model the session showed, but requests kept going to the pinned model. The switch is now also recorded as the session tree's temporary choice for that agent, which outranks the pin. New sessions still start on the pinned model.
- **Models of providers added at runtime can become an agent's default.** The agent-model control checked models against the startup provider routes. Picking a model from a provider added or refreshed after startup (for example with the Provider View's wizard) failed with `unavailable`, and `/model` reported an error. It now checks the engine's live routes.

## Features

- **One `/agent` view for every agent.** `/agent` now opens a full-screen Agents view instead of a picker. It has three sections, each under a titled divider: primary agents, subagents, and system agents (`compaction`, `summary`, `title`, which were hidden before). Enter selects a primary agent for the session. `m` opens the agent's model list and `t` its effort list. `c` clears a remembered model, and `r`, `/`, and Esc work as before. `/agent <name>` still switches directly. The `/agent-models` command and its view are removed; this view replaces them.
- **Change a pinned agent's model from the TUI.** Picking a model with `m` for an agent pinned in `config.yaml` (`agents.<id>.model`), or in a bundle's `config.yml`, writes that file and takes effect without a restart. New rpc `AgentModels.SaveAgentModelConfiguration` (`PUT /v1/agent-models/{agent_id}/configuration`, body `{directory?, session?, model?}`; omitting `model` clears the entry). `AgentModelState` gains `configurationPath`.
