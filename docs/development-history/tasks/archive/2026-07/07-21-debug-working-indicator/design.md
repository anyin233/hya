# Working indicator status design

## Scope

Repair the missing lifecycle publication in the synchronous Compat message
handler used by the current TypeScript prompt composer. Keep the frontend
renderer, sync reducer, event schema, projection, engine, and run registry
unchanged.

## Current Flow

```text
Prompt composer
  -> pinned SDK client.session.prompt
  -> POST /session/{id}/message
  -> RunRegistry::start
  -> SessionEngine::run_turn_with_external_dirs
  -> response
```

The frontend renders working state from `session.status`, but this route emits
no status event. `RunRegistry` can answer a polled bootstrap request while a run
is active; it does not update an already-running reactive client.

## Status Ownership

Keep publication at the route that owns the `RunGuard`, matching the existing
async prompt route. Make its existing `publish_session_status` helper visible to
sibling Compat modules and reuse it from the synchronous handler.

The synchronous `!no_reply` sequence is:

1. Acquire the existing `RunGuard`; retain the existing busy response if that
   fails.
2. Publish `busy` before resolving and running turn work.
3. Await the turn into a result without `?`.
4. Drop the guard so `/session/status` no longer reports the session as busy.
5. Publish `idle`.
6. Propagate the original turn result and preserve the existing response body.

This ordering makes success, provider/tool failure, and application abort clear
the reactive status. `noReply` and rejected concurrent prompts do not publish a
false lifecycle.

## Public Test Seam

Add one focused case to `packages/hya-tui-ts/test/real-backend.test.ts`:

- start the real `hya-backend` and subscribe through the pinned SDK global SSE
  client before prompting;
- create a session and call `client.session.prompt`, not `promptAsync`;
- wait for that session's `busy` event, await the prompt, then wait for `idle`;
- assert the filtered status sequence is exactly `busy`, `idle`.

The focused RED/GREEN command is:

```sh
cargo build -p hya-backend --bin hya-backend && bun test --cwd packages/hya-tui-ts test/real-backend.test.ts -t "pinned SDK synchronous prompt publishes busy then idle status"
```

Before the product edit, the accepted failure is the timeout waiting for
`busy`; build, startup, or SSE connection failures are not valid RED results.

## Compatibility And Limits

- No API response, persisted schema, event shape, or frontend rendering change.
- Existing session-status events remain append-only and replayable through the
  established publisher.
- The v2 prompt route remains unchanged because the current composer does not
  use it.
- If the HTTP handler future itself is dropped, Rust `Drop` releases the run but
  cannot asynchronously publish `idle`. That transport case is deferred until
  reproduced; application abort completes through the awaited result path.

## Release And Rollback

Ship as `0.33.16`, archive the `0.33.15` changelog, and keep root
`CHANGELOG.md` limited to the new patch. Rollback is the inverse atomic diff and
reinstallation of `0.33.15`; no database migration or data rollback is needed.
