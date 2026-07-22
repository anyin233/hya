# Tool-Round Limit Research

- Enforcement: `crates/hya-core/src/engine/turn.rs`, `SessionEngine::run_turn_rounds`.
- Current guard: `MAX_TOOL_ROUNDS = 25`; after each tool-bearing provider round, it emits synthetic stop text and returns `FinishReason::Error` when the count reaches 25.
- Scope: the same method serves root agents and subagents.
- Preserve: `rounds` remains necessary for `StepStarted`/`StepFinished` numbering.
- Excluded controls: goal/loop iterations and `SubagentLimits` count other work and do not enforce per-agent tool calling.
- Regression fixture: `FakeProvider::scripted_turns` in `crates/hya-provider/src/fake.rs`; existing integration-test home is `crates/hya-core/tests/turn_loop.rs`.
- Minimal proof: 26 tool-bearing scripts followed by explicit final text/stop; current code returns Error before consuming the final script.
