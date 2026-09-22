# 0.37.7

## Everything as Bundle runtime and presets

- Prepare core-agent definitions and native tool exposure from trusted `hya/core-agents` and `hya/base-tools` presets; preserve reserved identities, aliases, permissions, and compatibility names.
- Start bundle process and MCP providers before atomic runtime publication. Preserve old bindings across install/remove, retain packaged resources for process lifetime, and keep agent tools/hooks owner-scoped.
- Add declarative `AgentSetBundle.channels` policies for unit and parent-DM communication, with restrictive send/report/steer/follow-up and resident wake capabilities.
- Supply transient and resident worker definitions through `hya/subagents` and goal/evaluator/verifier/planner resources through `hya/goal-loop`; retain engine stop authority and model fallback.
- Import Claude plugin directories and local/Git marketplace entries into standard Plugin or AgentSetBundle packages with packaged source closure and supported native hook mappings.
- Extend functional coverage for process/MCP execution, scoped hooks, URI scheme reads, Claude import, runtime rollback, presets, and immutable bindings.
