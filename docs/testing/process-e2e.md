# Process E2E harness (`crates/hya-e2e`)

Track P runs the **production** backend binary against a temp XDG config and a
local scripted OpenAI-compatible chat-completions server (FakeLlm). No live
model keys are required. Every product assertion goes through the `hya.v1`
contract (the typed `hya-client` plus raw `/v1` JSON), so the matrix exercises
the same API real frontends consume.

## Layout

| Path | Role |
| --- | --- |
| `src/backend.rs` | Temp dirs, `config.yaml`, MCP/skill/bundle fixtures, spawn `hya serve` on `127.0.0.1:0` |
| `src/fake_llm.rs` | Queue of `ScriptStep::Text` / `ToolCalls` over SSE `/v1/chat/completions` |
| `src/scenario.rs` | `E2eEnv` / `E2eEnvBuilder`, HTTP helpers, permission/question auto-reply, tree helpers |
| `tests/p01_*.rs` … `p20_*.rs` | One scenario family per file (`p01`–`p20`, including `p12_context_api` through `p20_model_catalog_discovery`); run alone with `cargo test -p hya-e2e --test pNN_…` (two digits, e.g. `p20_model_catalog_discovery`) |
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

### Multi-agent scripting (routes)

One shared queue cannot drive two live agents: whichever asks first pops the
step. `E2eEnvBuilder::route(marker, steps)` pins a queue to the agent whose
**system prompt** contains `marker`, and records only that agent's requests.

```rust
let env = E2eEnvBuilder::new()
    .scripts(root_steps)                         // unrouted requests, as before
    .route("SYS_MARKER_A", vec![text_step("A1")])
    .build().await?;
env.wait_route_requests("SYS_MARKER_A", 1, timeout).await?;  // agent is running
env.wait_route_contains("SYS_MARKER_A", "needle", timeout).await?;
let dump = env.route_dump("SYS_MARKER_A")?;
```

Rules:

- Attribution reads **`system`-role content only**. A marker elsewhere in the
  transcript (tool-call arguments, mail bodies) is echoed into the *caller's*
  history too, so whole-body matching lets a caller steal its callee's queue.
  Give each teammate a unique system prompt via `task`'s `inline_agent.prompt`.
- An **exhausted route does not fall back** to the shared queue; the agent stops
  cleanly instead of eating another agent's steps.
- With no routes registered, dispatch is byte-identical to the shared queue.
- `route_remaining(marker)` must reach `0`. A route that never drains means the
  marker matched the wrong agent, or none.

Mailbox delivery only reaches **resident** teammates (`task` with
`resident: true`): `hya-core::resident` injects a handle's unread inbox into its
next turn as `[mail from <handle>] <body>` user prompts. There is no mailbox HTTP
route, so the recipient's own next request is the only delivery oracle — see
`crates/hya-e2e/tests/p16_swarm_mailbox.rs`.

Helpers:

- `fake_requests_from(&requests, 1)` — dump later turns as one string
- `env.prompt_with_permission_reply(session, text, "once"|"reject", timeout)`
- `env.prompt_with_question_reply(session, text, answers, timeout)`
- `env.wait_mcp_connected("echo", timeout)` — polls `GET /v1/mcp` for
  `connected`
- `env.session_context` (wraps `GET /v1/sessions/{id}/messages`) /
  `env.compact_session` (v1 compact) / `env.summarize_session_legacy`
  (name kept for history; hits `POST /v1/sessions/{id}/summarize`)
- `env.compat_create_session` / `env.compat_prompt_and_wait` — historical
  names; both are v1-backed (`POST /v1/sessions`, admit + wait through
  `POST /v1/sessions/{id}/turns`) and `compat_prompt_and_wait` drives the
  AGENTS-guidance path
- `env.session_todos` / `env.wait_session_idle`
- `tree_children` / `tree_max_depth` / `tree_session_ids` / `tree_subagent_types`
  for the session tree that `env.session_tree` assembles from v1
  parent-filtered session listings (`GET /v1/sessions?parent={id}`)

### Context management notes

- The v1 turn path (`POST /v1/sessions/{id}/turns`) is event-driven: it admits
  the turn and progress arrives on the event stream; the synchronous helpers
  above wait for the terminal state. Prompt and command turns run
  `run_turn_with_external_dirs_and_guidance` after discovering workdir
  `AGENTS.md` — use `compat_prompt_and_wait` for T1.13-style guidance tests.
- **Compact** (`POST /v1/sessions/{id}/compact`) and summarize
  (`POST /v1/sessions/{id}/summarize`) call `ModelSummarizer`, which hits the
  same FakeLlm as normal turns — script an extra `text_step` for the summary
  body before any post-compact turn.

## Oracle rules (do not weaken)

| Feature | Prefer |
| --- | --- |
| Filesystem tools | Disk side effects under the project dir |
| Permissions | File present/absent after once/reject |
| Skills | Follow-up FakeLlm body contains skill body marker (not only skill name) |
| MCP | Follow-up body contains MCP success text (e.g. `echo:…`); wait until `GET /v1/mcp` reports `connected` |
| Subagents | Session-tree children ≥ 1 (v1 parent-filtered listing), `subagent_type`, distinct child session ids |
| Nested | `tree_max_depth >= 2` and ≥ 3 distinct session ids |
| Hyabundle CLI | `bundle install/list/info/uninstall` stdout markers |
| Package agent | `GET /v1/agents` lists package agent; event text matches scripted final token |
| Session context | `GET /v1/sessions/{id}/messages` includes multi-turn user/assistant content |
| Project AGENTS.md | Guided FakeLlm request contains AGENTS body (not just the raw prompt text) |
| Compact / summarize | Context contains summary marker after compact; follow-up turn still works |
| Todo | `GET /v1/sessions/{id}/todo` lists items written via `todowrite` |
| Edit | Disk file content after `edit` tool |
| Mailbox `send` | The **recipient's** next FakeLlm request contains `[mail from …] <body>` — never the sender's success string or call args |
| Mailbox `send` receipts | A *direct* send hard-codes `recipients: 1`; only a `#channel` send counts real subscribers. Any receipt assertion must name the channel (`to #squad (N recipient…)`), never bare `recipients:1` |
| Mailbox `roster` / `channels` | Follow-up request carries state the caller never supplied (teammate session id, channel membership) |
| Mailbox `leave` | Negative claim needs both controls: a still-subscribed member that *did* receive the post, and a later direct ping the departed member *did* receive |

Avoid asserting only FakeLlm request counts or substring matches that appear in
tool-call arguments without results.

## Hyabundle fixtures

Public package bytes live under
`crates/hya-bundle/tests/fixtures/packages/valid_public_bundle_copy.7z`.
Install requires a path ending in `.hyabundle` — use
`materialize_public_bundle(dest_dir)` to copy with the correct suffix.

## Running

```sh
cargo build -p hya-backend --bin hya
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
