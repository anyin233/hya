# 0.37.9

## Provider connection in OpenTUI

The TUI now guides provider setup with `/connect deepseek` and
`/connect custom <id> <base-url> <model-id>`. A preview shows the endpoint,
models, default model, and whether a matching key is saved. Enter saves the
non-secret route through the new `Catalog.ConfigureProvider` v1 operation;
Esc cancels. Restart the backend to load the route and use the saved key.
