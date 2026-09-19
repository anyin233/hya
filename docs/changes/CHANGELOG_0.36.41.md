# 0.36.41

## `hya-sdk-v1`: the typed SDK for new frontends (sdk)

- New crate `hya-sdk-v1` — the successor client for frontends built on
  the consolidated contract. Protojson-typed over `hya-api` types:
  bootstrap snapshot, session create/get/list, event-driven turns
  (`create_turn`/`wait_turn` plus a synchronous `prompt` convenience),
  transcript/todo reads, curated event replay, pending-interaction
  list/respond, and the live per-session SSE subscription decoding
  typed `StreamFrame`s (with `resync` surfaced for re-replay).
- `V1SessionMirror` folds the curated frames into an in-memory
  transcript view (messages, parts, streaming text deltas) — the
  building block the new TUI's store layer can sit on.
- The legacy `hya-sdk` remains untouched for the current TUI and dies
  with the Compat surface in the deletion phase, as planned.
- Verified end to end against a live `/v1` server in-process: create →
  prompt to terminal state → SSE frames folded into the mirror with the
  streamed assistant text present.
