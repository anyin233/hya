# 0.43.13

## Subagent thinking effort: per-spawn, per-Agent defaults, runtime switching

- `task` takes an `effort` (top level and per member) that sets the spawned subagent's thinking effort; a `model: provider/model#level` suffix works too, and an explicit `effort` wins. A level the child's model does not accept fails the call with `INVALID_EFFORT` and spawns nothing. The result names the child's model with its suffix (`<task id="…" model="provider/model#low" state="…">`).
- Every Agent — the main agent and each subagent — can have its own default effort, independent of its model. Precedence: explicit suffix > the user's runtime choice (new SQLite table `agent_effort_preference`) > `agents.<id>.reasoning` in the owning configuration file > the bundle's authored `model_policy.reasoning` > per-model preference > model default > global `reasoning:` > none. It resolves per request, so a change applies to the Agent's next request without a restart.
- `list_agents` reports each Agent's `effort` and `effort_source` (`preference`, `configured`, `authored`) so the main agent can choose a spawn `effort` knowingly.
- New rpc `AgentModels.SetAgentEffort` (`PUT /v1/agent-efforts/{agent_id}`, body `{effort, directory}`, empty `effort` clears); `AgentModelState` gains `effort` and `effortSource` (`AGENT_EFFORT_SOURCE_PREFERENCE|CONFIGURED|AUTHORED|NONE`). `SessionInfo.effortSource` now reports `EFFORT_SOURCE_AGENT`.
- TUI `/agent-models` shows an `EFFORT` column; `e` opens the effort picker for the highlighted Agent (`default` clears the runtime choice).
- Fix: the store no longer fails to open with `UNIQUE constraint failed: _sqlx_migrations.version`. The daemon-handoff migrations merged after 0.43.12 reused version 15; they are now 16–18 (`pending_resume`, `pending_interaction`, `pending_interaction_reply`) after the released `0015_model_effort_preference`, and the new `agent_effort_preference` table is 19. A database created by an unreleased hot-reload branch build must be recreated.

```yaml
agents:
  explore:
    reasoning: low   # in ~/.config/hya/config.yaml
```

```json
{"description": "map the parser", "prompt": "find every entry point", "subagent_type": "explore", "effort": "high"}
```
