# 0.43.23

## Breaking changes

- **Fewer built-in agents, all prefixed `hya-`.** The 15 core agents plus the `hya/subagents` worker are now 8. The design follows oh-my-pi's presets (task, scout, reviewer):

  | New id | Role | Replaces |
  | --- | --- | --- |
  | `hya-main` | primary, default | `build`, `hya-main` |
  | `hya-plan` | primary, read-only | `plan`, `hya-planner` |
  | `hya-task` | subagent, `task` default | `general`, `hya-worker`, `hya-implementer`, `hya-tester`, `hya-docs` |
  | `hya-scout` | subagent, read-only | `explore`, `hya-explorer` |
  | `hya-reviewer` | subagent, read-only | `hya-reviewer` |
  | `hya-compaction`, `hya-summary`, `hya-title` | reserved system agents | `compaction`, `summary`, `title` |

  `hya-release` is removed. The `hya/subagents` first-party bundle is removed, so eleven first-party bundles remain. The workflow-private agents are renamed to `hya-goal-guide`, `hya-goal-verifier`, and `hya-pir-planner`, `hya-pir-implementer`, `hya-pir-reviewer`. Rename `agents.<id>` keys and `default_agent` in `config.yaml` to the new ids (see `docs/core-agents.md`).
- **Read-only agents are enforced at the tool layer.** Built-in agents now honor the core-agents preset's `resource_view`. `hya-plan` has no `write`, `edit`, `apply_patch`, or `bash`. `hya-scout` also lacks `task` and `archive`. `hya-reviewer` has no `write`, `edit`, or `apply_patch`.
- **Stored sessions keep working.** A session whose recorded agent no longer exists continues as `hya-task` on its next turn. The switch is recorded as `AgentSwitched`, so every client shows it. Explicitly spawning an unknown agent still fails.
