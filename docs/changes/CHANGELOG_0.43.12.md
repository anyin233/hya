# 0.43.12

## Thinking effort: one resolver, saved preferences, global default, `--effort`

- Every request resolves its effort with one precedence: `model#variant` suffix > authored Agent `model_policy.reasoning` > the user's saved per-model preference > the model's `reasoning.default` > the new top-level `reasoning:` config key > none (effort omitted). It is resolved each round, so a preference saved mid-session applies to the next request without a restart.
- Saved preferences live in the session database (`model_effort_preference`), are shared by every client of the backend, and are exposed as `AgentModels.ListModelEffortPreferences` (`GET /v1/model-effort-preferences`) and `AgentModels.SetModelEffortPreference` (`PUT /v1/model-effort-preferences/{provider_id}/{model_id}`, empty `effort` clears).
- `SessionInfo` gains `effectiveEffort` and `effortSource` (`EFFORT_SOURCE_SUFFIX|PREFERENCE|MODEL_DEFAULT|GLOBAL_DEFAULT|NONE`) computed by the same resolver, so the TUI header shows what the next request sends (`thinking high (pref)`).
- TUI `/effort <level>` now saves the server-side preference for the current model (dropping any `#suffix`); `/effort default` clears it. The client-side `thinkingEfforts` cache in `tui.json` is removed.
- New global `--effort <level>` flag appends `#level` to `--model` for `exec`/`run`/`-p`/`loop`; combining it with a model that already has a `#suffix` is an error.
- A spawned subagent that runs on a different model no longer inherits its parent's effort; the root Agent no longer bakes the model default in (which previously outranked any preference).

```yaml
reasoning: medium   # global fallback in ~/.config/hya/config.yaml
```

```sh
hya exec --model openai/gpt-6-astra --effort high "summarize the diff"
```
