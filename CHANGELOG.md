# 0.43.19

## Default agents and remembered `/model` choices

- New sessions created by the TUI now let the backend apply `default_agent` and each agent's configured `model` instead of selecting the first catalog row in the client.
- Switching agents remains session-local, so a later startup returns to the configured default agent.
- `/model provider/model` persists the model for the active agent; `/model provider/model#effort` persists both the model and that agent's default effort.
