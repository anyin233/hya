# Agent feature matrix

Process E2E lives in `crates/hya-e2e` (Track P): real `hya-backend` + scripted
OpenAI-compatible FakeLlm. Existing in-process tests remain the authority for
deep engine semantics (Track I); they are indexed, not duplicated.

Machine registry: `crates/hya-e2e/matrix.toml`.

## How to run

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1

# Track T (non-PTY real-backend / presentation)
cd packages/hya-tui-ts && bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

## Track P scenarios (implemented)

| ID | Title | Test |
| --- | --- | --- |
| T0.1 | Backend boots | `tests/p01_session_prompt.rs` |
| T1.2 | Prompt + FakeLlm text | `tests/p01_session_prompt.rs` |
| T1.3 | Multi-round tool loop | `tests/p02_tool_loop_fs.rs` |
| T1.4 | read/write | `tests/p02_tool_loop_fs.rs` |
| T1.5 | shell | `tests/p02_tool_loop_fs.rs` |
| T1.7 | Permissions once/reject | `tests/p03_permissions.rs` |
| T1.8 | Questions + reply | `tests/p04_questions.rs` |
| T1.9 | Skills load | `tests/p05_skills.rs` |
| T1.10 | MCP tool call | `tests/p06_mcp.rs` |
| T1.11 | Session list + resume | `tests/p07_session_lifecycle.rs` |
| T2.1 | Subagent task | `tests/p08_subagent_task.rs` |
| T2.2 | Nested tree depth≥2 | `tests/p09_nested_subagent.rs` |
| T2.3 | Agent roster / roles | `tests/p10_agent_roster.rs` |
| T2.7 | Hyabundle CLI lifecycle | `tests/p11_hyabundle.rs` |
| T2.8 | Hyabundle spawn agent | `tests/p11_hyabundle.rs` |

## Track T scenarios (implemented)

| ID | Title | Test |
| --- | --- | --- |
| T3.1 | Real-backend permission reply | `packages/hya-tui-ts/test/real-backend.test.ts` |
| T3.2 | Multi-agent task presentation | `packages/hya-tui-ts/test/task-presentation.test.ts` |
| T3.3 | Real-backend agent roster | `packages/hya-tui-ts/test/real-backend-agents.test.ts` |

## Track I (index-only)

| ID | Title | Path |
| --- | --- | --- |
| I.nested | Nested spawn tree | `crates/hya-app/tests/nested_spawn_tree.rs` |
| I.subagent | Subagent/resident core | `crates/hya-core/tests/subagent.rs` |
| I.permission_api | Compat permission/question | `crates/hya-server/tests/compat_permission_question_api.rs` |
| I.mcp_api | Compat MCP | `crates/hya-server/tests/compat_mcp_api.rs` |
| I.bundle_cli | Bundle CLI | `crates/hya-backend/tests/bundle_cli.rs` |
