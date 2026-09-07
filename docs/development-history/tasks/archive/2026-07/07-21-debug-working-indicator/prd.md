# Debug working indicator visibility

## Goal

Make the current TUI working indicator reliably show while the main agent is
actively processing a turn and clear when that work ends.

## Background

- The user reports that the working indicator is still not visible in the
  freshly installed `0.33.15` build.
- Commit `c1be2ad4` previously attempted to preserve agent status and reasoning
  effort, but its working presentation applies to subagents rather than the main
  session.
- The repository was clean at `6979ad15` when this investigation began.
- The current TypeScript prompt already renders non-idle session status and its
  sync reducer already consumes live `session.status` events.
- Normal composer submission uses the synchronous Compat
  `POST /session/{session_id}/message` route. That route runs the turn without
  publishing the `busy` and `idle` events emitted by the async prompt route.

## Requirements

- Add one deterministic pinned-SDK regression using `client.session.prompt` and
  the decoded global event stream used by the current composer.
- Publish the existing `busy` status only after the synchronous route acquires
  the session run and publish `idle` after that run is released.
- Publish `idle` before propagating a turn error so the TUI cannot remain stuck
  busy after provider, tool, or application-abort completion.
- Reuse the existing session-status event contract and publisher; do not add a
  second activity store, event type, or frontend state path.
- Preserve `noReply`, already-busy, response, status, reasoning-effort, and
  model-variant behavior.
- Rebuild and reinstall the verified version into `~/.local` after repository
  checks pass.
- Release the fix as patch version `0.33.16` with aligned workspace, frontend,
  README, lockfile, and one-version changelog metadata.

## Acceptance Criteria

- [x] The focused pinned-SDK command is observed failing before the product edit
  because the synchronous prompt produces no live `busy` event.
- [x] The same command passes after the fix and observes exactly `busy`, then
  `idle`, for the prompted session.
- [x] Existing prompt, TUI sync, `noReply`, busy-session, and error behavior
  remains passing.
- [x] The full required verification gate for touched areas passes.
- [x] `~/.local/bin/hya --version` reports the verified `0.33.16` build.

## Out Of Scope

- Redesigning the status line or adding unrelated activity states.
- Moving status persistence or event-bus dependencies into `RunRegistry` or
  `SessionEngine`.
- Adding status publication to the unused v2 prompt route without a reproduced
  client path.
- Adding async cleanup for an externally dropped HTTP handler unless a transport
  cancellation regression reproduces stale UI status.
