# 0.36.29

## Compaction escalates spill-first and summarizes structurally (core)

- The turn loop now walks a fixed three-rung ladder — spill tool outputs,
  provider compact, summarize — and stops at the first rung that brings the
  transcript back under threshold. A native compact that succeeded but stayed
  over threshold now escalates instead of ending the sequence with the request
  still over its window, and a turn that spilling alone rescued no longer pays
  for a summarizer call. After any rung replaces the transcript the count is
  re-derived from the tokenizer, because the provider's reported usage then
  describes a prompt that no longer exists.
- The trigger is reserve-aware: `min(window * context_fraction, window -
  reserve_tokens)` floored at a minimum, so the response always has room.
  New `compaction:` settings: `reserve_tokens` (default 16384) and
  `summary_max_tokens` (default 4096), both overridable through the existing
  `HYA_COMPACTION_*` env pattern. The hardcoded 1024-token summarizer cap is
  gone — it cut structured summaries off exactly where the pending-work and
  next-step sections live.
- The silent request-local summarization branch is deleted. It re-derived its
  trigger from the flat 100k `token_threshold`, so any turn between 100k and
  the window-scaled threshold was summarized every round without being
  persisted.
- The summarizer now sees what the agent did: `render_for_summary` renders
  tool name, bounded input, bounded output, and reasoning (2000-byte per-part
  budget) in addition to prose. `ModelSummarizer` fills an explicit
  eight-section template and anchors on the previous summary when one exists,
  so compaction is incremental rather than a fresh lossy pass.
- `/compact` now writes the `HYA_COMPACTED_CONTEXT` marker. `compacted_messages()`
  slices the transcript at that marker, so the command previously never
  produced a cut point and grew the context it was asked to shrink. The three
  independent marker definitions collapsed onto the canonical `hya_provider`
  constant.

## Tool output spills to `artifact://` behind a handle URL index (tool)

- New `crates/hya-tool/src/handle/` module resolving `artifact://<id>`,
  `skill://<name>`, and `local://<relpath>` URLs, with `?lines=N-M`, `?head=`,
  `?tail=`, `?grep=`, and `?q=` projections applied after hooks.
- Every oversized default tool result is now stored whole to the session
  artifact store before capping, and the truncation notice names its
  `artifact://` handle — spill-and-pointer for every tool at once, not just
  Bash. Coding envelopes deliberately do not spill: they already describe a
  file addressable by path.
- Compaction's spill rung makes evicted output recoverable the same way: the
  evicted body moves to the store and the transcript keeps an `artifact://…`
  pointer instead of a lossy "re-run the tool" notice, with a 512-byte floor
  so spilling a body smaller than the notice replacing it cannot grow the
  request.
- `read` resolves the internal schemes additively and `write` accepts
  `local://` scratch paths; every plain filesystem path behaves exactly as
  before. A handle is presented through the same path-based code as a file —
  permission checks, line numbering, paging — so the two cannot drift.
- `ArtifactHook` (`SessionEngine::with_artifact_hooks`) is the user-pluggable
  post-processing surface. Hooks chain in registration order and run on
  retrieval, never on write, so captured bytes stay authoritative and a hook
  that turns out to be wrong cannot already have destroyed the output.
  Artifact writes are staged and published by rename at mode 0600 with a
  `.meta.json` sidecar recording the producing tool.

## Exiting the TUI no longer fails the process (TUI)

- Quitting with `ctrl+c` / `ctrl+d` now exits `0` and leaves the terminal clean.
  It previously exited `1` and printed an `AbortError` and its stack over the
  just-restored terminal, on every exit.
- Cause: teardown aborts the SDK event stream, and the generated SSE client
  calls `reader.cancel()` from its own abort listener inside a `try`/`catch`
  that only guards synchronous throws. The returned promise rejects with nobody
  awaiting it, and Bun fails the process on the stray rejection.
- `destroyRenderer` now records that teardown has begun, and the CLI entry
  ignores exactly that late `AbortError`. Rejections outside teardown, and every
  other rejection reason, still fail the process as before.
- This unblocked five of the six PTY end-to-end cases, which assert a `0` exit
  and a restored terminal mode.

## Refused tool calls read as denied, not failed (TUI)

- A tool call the user rejects at the permission prompt now renders as a muted
  card instead of a red one. Denial is not a failure, and the red frame made
  every declined call look like something broke.
- `toolCardState` matched invented error markers that no backend path emits, so
  the `denied` state was unreachable. It now matches `permission denied`, the
  text of `PermissionError::Denied` in `crates/hya-tool/src/permission.rs`,
  which covers both an explicit deny rule and a user reject.
- `permission channel unavailable` stays an error: an ask channel that dropped
  is an infrastructure failure, not a decision.
- `toolCardState` and `toolCardError` now have unit coverage; they had none,
  which is why the unreachable state went unnoticed.
