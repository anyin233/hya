# 0.37.5

## AgentSetBundle packages

Public `AgentSetBundle` packages distribute one or more agents with shared
bundle resources without requiring a Workflow graph. The closed YAML schema,
canonical prepared-v2 encoding, catalog lookup, package writer, and existing
bundle install/list/search/info/uninstall path support the new payload.
Empty or duplicate rosters and unknown fields fail validation. Agent permissions
and late-bound `can_spawn` allowlists retain the existing bundle behavior.

Built-in agent migration and channel declarations are follow-up work.
