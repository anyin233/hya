# 0.43.33

## Features

- The global Commands overlay now puts its input bar first, with matching command and argument recommendations dropping down below it. The input stays at a fixed position as the suggestion list changes.

- `hya models --refresh` and `hya provider list --refresh` fetch through the database's running backend when one runs. The backend fetches each provider's model list once, writes the model cache, and publishes `catalogUpdated`, so the TUI and WebUI attached to it offer the refreshed models at once, without `hya serve restart`. Without a running backend the command fetches the lists itself, as before.
