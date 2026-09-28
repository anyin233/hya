# 0.43.10

## Compaction works inside one long turn

- A long agentic turn is a single assistant message. Every compaction rung used to require a foldable range of whole messages outside `keep_recent`, so a turn with hundreds of tool rounds never compacted, not even by moving old tool outputs out of the request.
- The `shake` rung (`SpillToolOutputs`) now also works per tool step: once the request is over the threshold it moves completed tool outputs older than the most recent `keep_recent` tool steps to artifacts, including steps inside the running assistant message, in addition to outputs outside the last `keep_recent` messages as before. The transcript keeps `[tool output moved to artifact://…]` handles; `read` still resolves them to the full bytes.
- The summarizing and folding rungs still require a foldable message range; each rung now checks its own precondition.
