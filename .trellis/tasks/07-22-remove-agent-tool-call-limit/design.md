# Design

## Current Behavior

`SessionEngine::run_turn_rounds` streams a provider response, emits step events, executes every requested tool, increments `rounds`, and starts another provider round. After the 25th tool-bearing round, a dedicated branch emits synthetic stop text and an error-finished step/message instead of asking the provider for the next response.

## Change

Delete `MAX_TOOL_ROUNDS` and the branch that checks it. Keep `rounds`, `rounds += 1`, and all existing event emission because the counter is the persisted step number rather than only a limit counter.

The resulting flow exits only through existing conditions:

- cancellation at the start of a round;
- provider completion when a response has no tool calls;
- provider/store errors propagated by the engine;
- tool errors recorded as tool results before the provider decides whether to continue.

No public type, configuration format, event schema, provider contract, or subagent governor changes.

## Regression Boundary

Extend `crates/hya-core/tests/turn_loop.rs` with one integration test using existing `FakeProvider::scripted_turns` support:

1. Generate 26 responses containing an unknown tool call and `FinishReason::ToolCalls`.
2. Append an explicit response containing unique final text and `FinishReason::Stop`.
3. Assert the returned finish reason is `Stop` and the projected assistant message contains the unique final text.

The current implementation returns `Error` after the 25th tool-bearing round and never consumes the final response, giving a precise RED condition without new fixtures.

## Tradeoffs

Removing the guard intentionally permits an uncooperative model to continue consuming time and tokens. Existing cancellation, provider errors, token events, and externally bounded goal/loop modes remain the controls requested by the current architecture. A configurable replacement would preserve the behavior the user asked to remove and is therefore rejected.

The `u32` step number remains unchanged for wire compatibility. Its theoretical overflow after billions of rounds is not a practical replacement for the removed product limit and does not justify an event-schema migration.

## Release And Rollback

Bump the workspace patch version from `0.33.27` to `0.33.28`, archive the current root changelog verbatim, and write a new root changelog containing only the new version. Cargo may refresh local workspace package versions in `Cargo.lock`; dependencies must not be broadly updated.

The change is one atomic commit. Rollback is reverting that commit; no persisted-data or configuration migration is involved.
