# 0.43.22

## Fixes

- **`/model` now switches a pinned agent's session model.** When `config.yaml` pinned the session agent's model (`agents.<id>.model`), or a bundle policy did, `UpdateSession {model}` changed the model the session showed, but requests kept going to the pinned model. The switch is now also recorded as the session tree's temporary choice for that agent, which outranks the pin. New sessions still start on the pinned model.
- **Models of providers added at runtime can become an agent's default.** The agent-model control checked models against the startup provider routes. Picking a model from a provider added or refreshed after startup (for example with the Provider View's wizard) failed with `unavailable`, and `/model` reported an error. It now checks the engine's live routes.
