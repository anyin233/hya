# Working Plan

## Goal

Plan, implement, and verify OpenAI Responses support and end-to-end reasoning
effort propagation while retaining Chat Completions compatibility.

## Phases

- [completed] Inspect configuration, model selection, request encoding, stream
  decoding, and existing tests.
- [completed] Merge independent planner findings into `design.md` and
  `implement.md`; converge `prd.md`.
- [completed] Manifests validated; plan approved and Trellis task activated.
- [completed] Implement event replay, startup defaults, and the Responses
  request/stream/continuation behavior test-first with the smallest shared
  changes.
- [completed] Update documentation, version, and release notes.
- [completed] Run required full verification, commit, push, publish the
  release, and complete the Trellis finish workflow.

## Errors

- A broad documentation/config grep exceeded the tool's JSON record limit;
  narrowed subsequent inspection to named config/runtime symbols and files.
- Direct API-reference pages exceeded the web fetch size limit; the focused
  official reasoning guide and live schema probes supplied the required fields.
- `.trellis/scripts/workflow_phase.py` is not present in this checkout; use
  `get_context.py --mode phase` and `task.py validate` for the available Trellis
  planning gates.
