# 0.34.5

- Pin each admitted assistant or direct-shell turn to one immutable runtime
  generation across prompt skills, model-visible schemas, and tool dispatch.
- Build complete tool, skill, and MCP candidates off the active path and
  publish them with one atomic snapshot replacement; failed and no-op
  candidates preserve the current generation.
- Record the lightweight generation identity on each assistant message while
  keeping registry contents out of the event log and projection.
- Route deferred MCP tools through the single runtime publisher so candidate
  builders can no longer mutate an engine-visible registry.
