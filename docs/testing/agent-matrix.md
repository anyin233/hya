# Agent feature matrix

Process E2E lives in `crates/hya-e2e` (Track P): real `hya-backend` + scripted
OpenAI-compatible FakeLlm. Existing in-process tests remain the authority for
deep engine semantics (Track I); they are indexed, not duplicated.

Machine registry: `crates/hya-e2e/matrix.toml`.

## How to run

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```

## Track P scenarios (implemented)

| ID | Title | Test |
| --- | --- | --- |
| T0.1 | Backend boots | `tests/p01_session_prompt.rs` |
| T1.2 | Prompt + FakeLlm text | `tests/p01_session_prompt.rs` |
| T1.3 | Multi-round tool loop | `tests/p02_tool_loop_fs.rs` |
| T1.4 | read/write | `tests/p02_tool_loop_fs.rs` |
| T1.5 | shell | `tests/p02_tool_loop_fs.rs` |

## Planned (not yet in hya-e2e)

| ID | Title | Prefer |
| --- | --- | --- |
| T1.7–T1.8 | Permissions / questions | Track P non-yolo + TUI real-backend |
| T1.9–T1.10 | Skills / MCP | Track P |
| T2.1–T2.2 | Subagents / nested | Track P + index core |
| T2.7–T2.8 | Hyabundle CLI + spawn | Track P |
| T3.* | TUI presentation | bun `test/e2e` / pty-smoke |

## Track I (index-only examples)

- Admission capacity: `crates/hya-store/tests/r10_certification.rs`
- Nested spawn: `crates/hya-app/tests/nested_spawn_tree.rs`
- Resident/subagent: `crates/hya-core/tests/subagent.rs`
- Compat session/MCP/permission: `crates/hya-server/tests/compat_*.rs`
