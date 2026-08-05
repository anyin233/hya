# Agent feature matrix

Process E2E lives in `crates/hya-e2e` (**Track P**): real `hya-backend` +
scripted OpenAI-compatible FakeLlm. Existing in-process tests remain the
authority for deep engine semantics (**Track I**); they are indexed, not
duplicated. TUI/SDK coverage is **Track T**.

| Resource | Path |
| --- | --- |
| Machine registry | [`crates/hya-e2e/matrix.toml`](../../crates/hya-e2e/matrix.toml) |
| Harness guide | [process-e2e.md](process-e2e.md) |
| Testing overview | [README.md](README.md) |

## How to run

```sh
# Track P — process agent suite
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1

# Track T — non-PTY real-backend / presentation
cd packages/hya-tui-ts
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

## Track P scenarios (implemented)

| ID | Title | Test | Strong oracle (summary) |
| --- | --- | --- | --- |
| T0.1 | Backend boots | `tests/p01_session_prompt.rs` | HTTP responds on serve URL |
| T1.2 | Prompt + FakeLlm text | `tests/p01_session_prompt.rs` | Events contain scripted assistant text |
| T1.3 | Multi-round tool loop | `tests/p02_tool_loop_fs.rs` | Sequential write → read → shell |
| T1.4 | read/write | `tests/p02_tool_loop_fs.rs` | Disk file content |
| T1.5 | shell | `tests/p02_tool_loop_fs.rs` | Disk file from shell |
| T1.7 | Permissions once/reject | `tests/p03_permissions.rs` | File created only after `once` |
| T1.8 | Questions + reply | `tests/p04_questions.rs` | Turn continues after question reply |
| T1.9 | Skills load | `tests/p05_skills.rs` | `/skill` lists skill; follow-up FakeLlm has body marker |
| T1.10 | MCP tool call | `tests/p06_mcp.rs` | `/mcp` connected; follow-up has `echo:…` result |
| T1.11 | Session list + resume | `tests/p07_session_lifecycle.rs` | Compat list shows active sessions; second prompt on same id |
| T1.12 | Session context API | `tests/p12_context_api.rs` | Multi-turn user/assistant text in `/api/session/{id}/context` |
| T1.13 | Project AGENTS.md guidance | `tests/p13_project_agents_context.rs` | Compat-guided FakeLlm request contains AGENTS body marker |
| T1.14 | Compact / summarize | `tests/p14_compact_summarize.rs` | Compact injects summary into context; follow-up turn works |
| T1.15 | todowrite + edit | `tests/p15_todo_and_edit.rs` | Todo route lists item; edit rewrites file on disk |
| T2.1 | Subagent task | `tests/p08_subagent_task.rs` | Tree children ≥ 1, `general`, distinct child session |
| T2.2 | Nested tree depth≥2 | `tests/p09_nested_subagent.rs` | Depth ≥ 2, explore+plan, ≥ 3 session ids |
| T2.3 | Agent roster / roles | `tests/p10_agent_roster.rs` | `/api/agent` lists build + spawnable roles |
| T2.7 | Hyabundle CLI lifecycle | `tests/p11_hyabundle.rs` | install/list/info/uninstall stdout |
| T2.8 | Hyabundle spawn agent | `tests/p11_hyabundle.rs` | Roster has package agent; events have scripted text |

## Track T scenarios (implemented)

| ID | Title | Test |
| --- | --- | --- |
| T3.1 | Real-backend permission reply | `packages/hya-tui-ts/test/real-backend.test.ts` |
| T3.2 | Multi-agent task presentation | `packages/hya-tui-ts/test/task-presentation.test.ts` |
| T3.3 | Real-backend agent roster | `packages/hya-tui-ts/test/real-backend-agents.test.ts` |

Full PTY matrix for every feature ID is **not** required for the PR gate; PTY
remains presentation smoke (`pty-smoke.test.ts`).

## Track I (index-only)

Deep engine / API coverage owned by in-process suites. Process E2E does not
replace these.

| ID | Title | Path |
| --- | --- | --- |
| I.nested | Nested spawn tree | `crates/hya-app/tests/nested_spawn_tree.rs` |
| I.subagent | Subagent/resident core | `crates/hya-core/tests/subagent.rs` |
| I.permission_api | Compat permission/question | `crates/hya-server/tests/compat_permission_question_api.rs` |
| I.mcp_api | Compat MCP | `crates/hya-server/tests/compat_mcp_api.rs` |
| I.bundle_cli | Bundle CLI | `crates/hya-backend/tests/bundle_cli.rs` |
| I.context_api | Compat session context | `crates/hya-server/tests/compat_session_v2_context_api.rs` |
| I.compact_api | Compat compact | `crates/hya-server/tests/compat_session_v2_compact_api.rs` |
| I.compact_engine | Engine compact_context | `crates/hya-core/tests/compact_context.rs` |

## Adding a scenario

1. Prefer extending Track I if the regression is pure engine/API shape.
2. For product-path regressions (config, real binary, FakeLlm tool loop), add
   `crates/hya-e2e/tests/pNN_*.rs` using `E2eEnvBuilder`.
3. Register the ID in `matrix.toml` and this page.
4. Keep oracles honest — see [process-e2e.md](process-e2e.md#oracle-rules-do-not-weaken).
5. Run `cargo test -p hya-e2e --test pNN_… -- --test-threads=1` then the full
   crate suite before landing.
