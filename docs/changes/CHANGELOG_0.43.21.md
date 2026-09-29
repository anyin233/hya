# 0.43.21

## Fixes

- **`/model` on a config-pinned Agent no longer errors.** When `config.yaml` pins an Agent's model (`agents.<id>.model`), the backend rejects a remembered preference for it with `409 Conflict`. `/model` treated that as a failure after the session had already switched. It now keeps the choice as a session override and leaves the configured default in place, so new sessions still start on the configured model and effort.
