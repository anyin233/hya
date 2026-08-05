# Process E2E harness (`crates/hya-e2e`)

Track P runs the **production** backend binary against a temp XDG config and a
local scripted OpenAI-compatible chat-completions server (FakeLlm). No live
model keys are required.

## Layout

| Path | Role |
| --- | --- |
| `src/backend.rs` | Temp dirs, `config.yaml`, MCP/skill/bundle fixtures, spawn `hya-backend serve` on `127.0.0.1:0` |
| `src/fake_llm.rs` | Queue of `ScriptStep::Text` / `ToolCalls` over SSE `/v1/chat/completions` |
| `src/scenario.rs` | `E2eEnv` / `E2eEnvBuilder`, HTTP helpers, permission/question auto-reply, tree helpers |
| `tests/p01_*.rs` … `p11_*.rs` | One scenario family per file; run alone with `cargo test -p hya-e2e --test p0N_…` |
| `matrix.toml` | Machine-readable PR-matrix registry (IDs → paths) |

## Building an environment

```rust
let env = E2eEnvBuilder::new()
    .yolo(true)                      // default: auto-approve tools
    .permission_model("allow")       // or "default" / "strict" for ask paths
    .with_mcp_echo()                 // project fixtures/mcp_echo.py + config mcp.echo
    .skill_file(".hya/skills/…/SKILL.md", body)
    .preinstall_bundle(package_path) // hyabundle into XDG_DATA_HOME before serve
    .scripts(vec![
        tool_step("write", json!({ "filePath": "a.txt", "content": "x" })),
        text_step("DONE"),
    ])
    .build()
    .await?;
```

Isolation per process:

- Project workdir, HOME, `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / state / cache
- SQLite `--db` path
- FakeLlm base URL written as `providers.fake` openai-compatible endpoint

When MCP fixtures are present, the harness sets `HYA_DEFER_SIDEPLANES=0` so MCP
tools are registered before the first prompt (default serve defers MCP attach).

## Scripting FakeLlm

Each completion request pops one script step:

1. `Text("…")` — stream assistant text and stop.
2. `ToolCalls([...])` — stream function tool calls; the engine executes tools and
   calls the model again with tool results.

Exhausted scripts return a clean empty stop so agent loops terminate.

Inspect recorded bodies with `env.fake.requests()`. For tool **results**, assert
on the **follow-up** request (index ≥ 1), not the turn that emitted the tool
call — call args alone do not prove execution.

Helpers:

- `fake_requests_from(&requests, 1)` — dump later turns as one string
- `env.prompt_with_permission_reply(session, text, "once"|"reject", timeout)`
- `env.prompt_with_question_reply(session, text, answers, timeout)`
- `env.wait_mcp_connected("echo", timeout)`
- `env.session_context` / `env.compact_session` / `env.summarize_session_legacy`
- `env.compat_create_session` / `env.compat_prompt_and_wait` (AGENTS guidance path)
- `env.session_todos` / `env.wait_session_idle`
- `tree_children` / `tree_max_depth` / `tree_session_ids` / `tree_subagent_types`
  for `/session/{id}/tree`

### Context management notes

- **Native** `POST /sessions/:id/prompt` is synchronous and does **not** inject
  per-turn AGENTS/reference guidance (server AppState keeps agent base only).
- **Compat** `POST /api/session/:id/prompt` is async and runs
  `run_turn_with_external_dirs_and_guidance` after discovering workdir
  `AGENTS.md`. Use `compat_prompt_and_wait` for T1.13-style tests.
- **Compact** (`POST /api/session/:id/compact`) and legacy summarize call
  `ModelSummarizer`, which hits the same FakeLlm as normal turns — script an
  extra `text_step` for the summary body before any post-compact turn.

## Oracle rules (do not weaken)

| Feature | Prefer |
| --- | --- |
| Filesystem tools | Disk side effects under the project dir |
| Permissions | File present/absent after once/reject |
| Skills | Follow-up FakeLlm body contains skill body marker (not only skill name) |
| MCP | Follow-up body contains MCP success text (e.g. `echo:…`); wait until `/mcp` is `connected` |
| Subagents | `/session/{id}/tree` children ≥ 1, `subagent_type`, distinct child session ids |
| Nested | `tree_max_depth >= 2` and ≥ 3 distinct session ids |
| Hyabundle CLI | `bundle install/list/info/uninstall` stdout markers |
| Package agent | `/api/agent` lists package agent; event text matches scripted final token |
| Session context | `/api/session/{id}/context` includes multi-turn user/assistant content |
| Project AGENTS.md | Compat-guided FakeLlm request contains AGENTS body (not only native prompt) |
| Compact / summarize | Context contains summary marker after compact; follow-up turn still works |
| Todo | `/session/{id}/todo` lists items written via `todowrite` |
| Edit | Disk file content after `edit` tool |

Avoid asserting only FakeLlm request counts or substring matches that appear in
tool-call arguments without results.

## Hyabundle fixtures

Public package bytes live under
`crates/hya-bundle/tests/fixtures/packages/valid_public_bundle_copy.7z`.
Install requires a path ending in `.hyabundle` — use
`materialize_public_bundle(dest_dir)` to copy with the correct suffix.

## Running

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
cargo test -p hya-e2e --test p03_permissions -- --nocapture
cargo clippy -p hya-e2e --all-targets -- -D warnings
```

`--test-threads=1` avoids port/process contention across concurrent backends.

## Related

- Scenario inventory: [agent-matrix.md](agent-matrix.md)
- CI wiring sketch: [ci-agent-e2e-snippet.yml](ci-agent-e2e-snippet.yml)
- In-process authority for nested spawn: `crates/hya-app/tests/nested_spawn_tree.rs`
- Bundle CLI unit coverage: `crates/hya-backend/tests/bundle_cli.rs`
