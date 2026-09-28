# 0.43.18

## Thinking effort: switch any time, shown as `model:effort`

- The TUI and WebUI show the effort in use right after the model name: the header reads `… build openai/gpt-6-astra:max …` and the status bar `mode manual · gpt-6-astra:max · …`. `:default` means no request effort (the provider decides); `/status` keeps the layer that chose it (`Thinking    max (pref)`).
- `/effort <level>` (and the picker) always takes effect: the choice is saved on the layer that decides the session's effort. A `#suffix` is dropped first; when the session's Agent has its own effort, the choice becomes that Agent's runtime effort instead of a model preference the Agent would outrank. `/effort default` clears both.
- Switching while a turn runs is supported: the label updates at once and the next request round uses the new effort.
- `SetModelEffortPreference` and `SetAgentEffort` now emit a live `catalogUpdated` frame, and the TUI re-reads its open session on it, so a switch in one client (the WebUI) shows in every other one (the terminal TUI) without a key press.
- The choice is stored by the backend (SQLite), so the next start comes back with it.

```text
/effort max     → Thinking effort → max    header: … build openai/gpt-6-astra:max …
/effort default → Thinking effort → default
```
