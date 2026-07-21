# Working indicator implementation plan

## Preconditions

- Activate the task only after the user approves this plan and the public test
  seam.
- Preserve unrelated workspace changes and stage only task-owned files.
- Reconfirm that `v0.33.16` remains unused before release metadata changes.

## Ordered Checklist

1. Add the focused pinned-SDK regression to
   `packages/hya-tui-ts/test/real-backend.test.ts`.
   - Subscribe to global events before creating and prompting the session.
   - Invoke `client.session.prompt` and filter status events by session ID.
   - Wait for `busy`, await the prompt, wait for `idle`, and assert the exact
     `busy`, `idle` sequence.
   - Run the focused command from `design.md` and record the expected RED timeout
     before changing production code.

2. Publish synchronous prompt lifecycle status.
   - Make the existing async-route `publish_session_status` helper
     `pub(super)`.
   - After successful `RunRegistry::start`, publish `busy`.
   - Capture the turn result, drop the guard, publish `idle`, then propagate the
     result.
   - Make no frontend, engine, registry, v2-route, or event-schema change.

3. Run the focused command to GREEN, then run the complete real-backend test
   file and the existing server prompt-status integration test:

```sh
bun test --cwd packages/hya-tui-ts test/real-backend.test.ts
cargo test -p hya-server --test compat_prompt_async_events_api
```

4. Update patch-release metadata after behavior is GREEN.
   - Move root `CHANGELOG.md` to
     `docs/changes/CHANGELOG_0.33.15.md`.
   - Write root `CHANGELOG.md` with only the `0.33.16` working-indicator fix.
   - Update `Cargo.toml`, generated `Cargo.lock` workspace package entries,
     `packages/hya-tui-ts/package.json`, and `README.md` to `0.33.16`.

5. Run the touched-area and repository verification gates:

```sh
bun test --cwd packages/hya-tui-ts
bun run --cwd packages/hya-tui-ts typecheck
bun run --cwd packages/hya-tui-ts build
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p hya -p hya-backend
```

6. Install and verify the release build:

```sh
./install.sh --prefix "$HOME/.local" --profile release
"$HOME/.local/bin/hya" --version
```

7. Review `git diff` and `git status`, run Trellis quality review, then commit,
   push, and archive only after every required check passes.

## Review Gates

- The RED failure is missing synchronous prompt status, not test setup.
- `busy` is emitted only after run ownership succeeds.
- The guard is dropped and `idle` is emitted before a turn error is propagated.
- `noReply`, busy rejection, response payloads, and async prompt behavior remain
  unchanged.
- No new lifecycle abstraction, dependency, event type, or frontend state path.
- Unrelated files remain untouched and unstaged.

## Rollback Points

- The test and two production edits are one independently reversible behavior
  slice before release metadata changes.
- If a required gate fails, record the exact blocker and do not commit, push, or
  replace the installed binary.
- The installer owns placement rollback; no persisted-data rollback is needed.
