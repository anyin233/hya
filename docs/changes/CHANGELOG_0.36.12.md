# 0.36.12

## Restore remembered models on TUI startup

- Use the backend's effective Agent model before legacy Agent metadata when a new TUI starts, so remembered choices control both the footer and the next provider request after restart.
- Preserve request-local selections, explicit launch overrides, configured Agent policy, and legacy-backend fallback.
- Cover cold-start precedence in the existing interactive model-selection regression.
