# Agent feature matrix

Process E2E lives in `crates/hya-e2e` (**Track P**): real `hya` +
scripted OpenAI-compatible FakeLlm, driven through the `hya.v1` contract and
headless `hya exec`. Existing in-process tests remain the authority for deep
engine semantics (**Track I**); they are indexed, not duplicated. Track P is
organized into 11 dense golden-path modules safe for parallel execution
(`--test-threads=4`), with heavy real-OS storage fault/soak scenarios gated
behind `#[ignore = "nightly OS fault/soak"]` for nightly runs.

| Resource | Path |
| --- | --- |
| Machine registry | [`crates/hya-e2e/matrix.toml`](../../crates/hya-e2e/matrix.toml) |
| Harness guide | [process-e2e.md](process-e2e.md) |
| Testing overview | [README.md](README.md) |

## How to run

```sh
# Track P — one consolidated module (local)
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e --test it <pNN_module>:: -- --test-threads=4

# Track P — whole PR matrix (parallel)
cargo test -p hya-e2e -- --test-threads=4

# Track P — nightly real-OS fault/soak tier (p43)
cargo test -p hya-e2e -- --ignored --test-threads=1
```

## Track P scenarios (implemented)

| ID | Title | Module | Strong oracle (summary) |
| --- | --- | --- | --- |
| T0.1 | Backend boots | `tests/it/p01_core_session.rs` | `/v1/health` responds on serve URL |
| T1.2 | Prompt + FakeLlm text | `tests/it/p01_core_session.rs` | Events contain scripted assistant text |
| T1.3 | Multi-round tool loop | `tests/it/p01_core_session.rs` | Sequential write → read → edit → bash → todo → webfetch → skill |
| T1.4 | read/write | `tests/it/p01_core_session.rs` | Disk file content + correlated `read` `ToolResult` |
| T1.5 | bash | `tests/it/p01_core_session.rs` | Disk file from shell |
| T1.7 | Permissions once/reject, `yolo` mode switch | `tests/it/p02_permissions_and_questions.rs` | File created only after `once`; switching to `yolo` unblocks pending ask and skips later asks |
| T1.8 | Questions + reply (`ask_user` + `question`) | `tests/it/p02_permissions_and_questions.rs` | Both canonical tool and legacy alias continue after reply; `ask_user` + `plan_exit` offered |
| T1.9 | Skills load | `tests/it/p01_core_session.rs` | `GET /v1/skills` lists skill; follow-up FakeLlm has body marker |
| T1.10 | MCP tool call | `tests/it/p03_mcp_and_custom_resources.rs` | `/v1/mcp` connected with tool list; follow-up has `echo:…` result |
| T1.11 | Session list + resume + titling + daemon stop/start restore | `tests/it/p01_core_session.rs` | `GET /v1/sessions` shows sessions; background titling runs once; `serve stop`/`start` restores interrupted tool & cancels unanswered permission; URI `--db` rejected |
| T1.12 | Session transcript read | `tests/it/p01_core_session.rs` | Multi-turn user/assistant text in `GET /v1/sessions/{id}/messages` |
| T1.13 | Project AGENTS.md guidance | `tests/it/p01_core_session.rs` | Guided FakeLlm request contains AGENTS body marker |
| T1.14 | Compact / summarize | `tests/it/p01_core_session.rs` | Compact adds rewrite marker with removed user messages and evicts turn 1; `/summarize` persists marker |
| T1.15 | Kernel todo + edit + bare exec JSONL/stdin/tail-session | `tests/it/p01_core_session.rs` | Todo route lists items, edit mutates file, `exec --bare` streams JSONL matching `tail-session` and reads stdin `-` |
| T1.16 | Custom slash catalog and route expansion | `tests/it/p03_mcp_and_custom_resources.rs` | Supported command sources, precedence, single-pass expansion, `CommandExecuted`, explicit text bypass |
| T1.17 | Skill-backed slash expansion | `tests/it/p03_mcp_and_custom_resources.rs` | Direct `<skill>` expansion without calling builtin `skill`, root precedence, literal fallback on removal |
| T1.18 | Custom command invokes builtin Skill Tool | `tests/it/p03_mcp_and_custom_resources.rs` | Real `skill` result reaches follow-up request; unknown-skill structured error recovers in same session |
| T1.19 | Custom command invokes plugin Tool | `tests/it/p03_mcp_and_custom_resources.rs` | `toolbox__remember` result, crash respawn, drift fail-closed, hot manifest reload |
| T1.21 | Custom resource conflicts fail closed | `tests/it/p03_mcp_and_custom_resources.rs` | Qualified plugin names alongside unique bare `read`; ambiguous `__` MCP registrations fail closed |
| T1.22 | Dynamic resource snapshots and reload | `tests/it/p03_mcp_and_custom_resources.rs` | Skill next-turn refresh with immutable old turns, MCP disconnect/connect reload, plugin manifest hot reload |
| T1.23 | Structured custom Tool errors recover | `tests/it/p03_mcp_and_custom_resources.rs` | Single terminal `ToolError`, replay never re-executes plugin (call log oracle), same-session recovery |
| T1.24 | Native network tool (`webfetch`) | `tests/it/p01_core_session.rs` | `webfetch` returns loopback HTTP 200 body in correlated `ToolResult` |
| T2.1 | Subagent task (single + batch) | `tests/it/p04_subagents_and_mailbox.rs` | Tree children ≥ 1, `hya-task`, distinct child ids, DM mail events, batch 2-member result |
| T2.2 | Nested tree depth≥2 | `tests/it/p04_subagents_and_mailbox.rs` | Depth ≥ 2, `hya-task` + `hya-plan`, ≥ 3 distinct session ids |
| T2.3 | Agent roster / roles | `tests/it/p04_subagents_and_mailbox.rs` | `GET /v1/agents` lists `hya-main` + spawnable roles; `list_agents` hides workflow-private agents; private spawn refused |
| T2.4 | Team roster + `list_channel` | `tests/it/p04_subagents_and_mailbox.rs` | `list_channel` follow-up surfaces live DM/group channels and team member handles |
| T2.5 | Mailbox `send` (vertical DM) | `tests/it/p04_subagents_and_mailbox.rs` | Parent's next FakeLlm request carries worker's DM payload |
| T2.6 | Mailbox `send` (unit broadcast) | `tests/it/p04_subagents_and_mailbox.rs` | Live child's next FakeLlm request carries lead's broadcast payload |
| T2.7 | Hyabundle CLI lifecycle | `tests/it/p06_bundle_lifecycle_and_runtime.rs` | `bundle install/list/info/uninstall` stdout markers |
| T2.8 | Hyabundle spawn package agent | `tests/it/p06_bundle_lifecycle_and_runtime.rs` | Roster lists installed package agent; events stream scripted text |
| T2.9 | Mailbox `list_channel` | `tests/it/p04_subagents_and_mailbox.rs` | Follow-up carries live DM/group channels and `hya-task-*` team state |
| T2.13 | User-authored Workflow fan-out/fan-in | `tests/it/p05_workflows.rs` | 4 distinct stage sessions, parallel impls joined into review (`<stage id="impl_a"` + `impl_b`), lead resumes |
| T2.14 | Workflow stage model routing and replay | `tests/it/p05_workflows.rs` | 3×503 fallback with per-candidate reasoning effort; state survives `env.reopen()` with zero extra requests |
| T2.15 | Model catalog discovery and offline fallback | `tests/it/p10_model_catalog_discovery.rs` | Pinned merge/cache, empty-list anonymous rediscovery, 401→`hya/offline` + exec explanation, mixed valid/failed/403 parity across CLI, `/v1/models`, `/v1/providers`, `/v1/bootstrap` |
| T2.16 | Agent model preference | `tests/it/p04_subagents_and_mailbox.rs` | `PUT /v1/agent-models/hya-task` steers spawned subagent model and reports `AGENT_MODEL_SOURCE_REMEMBERED` |
| T2.17 | Dispatch-time model resolution | `tests/it/p04_subagents_and_mailbox.rs` | Exact override, substring match, bare vendor deferral, and unspecified preference fallback |
| T2.19 | Installed bundle schemas | `tests/it/p06_bundle_lifecycle_and_runtime.rs` | `bundle schema`, `bundle info`, `read("db://...")`, and `GET /v1/runtime/schemas` claim |
| T2.20 | `hya/goal-loop` preset + patched evaluator | `tests/it/p07_bundle_policies_and_extras.rs` | Hidden workflow-private agents, `goal-contract` skill read, `goal.evaluate` injection reaches iteration 2 |
| T2.21 | Agentless Plugin static skill | `tests/it/p06_bundle_lifecycle_and_runtime.rs` | Static skill loads after install and disappears after uninstall |
| T2.22 | Packaged processes, JS plugins, MCP, hooks, live config | `tests/it/p06_bundle_lifecycle_and_runtime.rs` | Native process + MCP, owner-scoped `hook_refs`, agentless Bun JS, `chat.params` lineage, `model.fallback`, and live `config.yaml` reload on one backend PID |
| T2.23 | Default subagent bundle worker | `tests/it/p04_subagents_and_mailbox.rs` | Worker mails parent, calls `report`, takes no model round after accepted report, inherits runtime model |
| T2.24 | Claude import | `tests/it/p06_bundle_lifecycle_and_runtime.rs` | `--claude` import runs packaged `PreToolUse` hook and skill after source deletion; uninstall removes skill |
| T2.25 | Installed channel policy | `tests/it/p07_bundle_policies_and_extras.rs` | Channel bundle denies child sends without parent delivery; uninstall restores default delivery |
| T2.26 | `hya-extra/zvec-grep` and `scout` bundles | `tests/it/p07_bundle_policies_and_extras.rs` | Fake `zg` MCP server answers both `hya-main` and spawned `scout` subagent |
| T2.27 | `hya-extra/jev-model-router` | `tests/it/p07_bundle_policies_and_extras.rs` | Routes `chat.params` to Jev-chosen tier, stays sticky per chain, falls back to `default_tier` on HTTP 500 |
| T2.28 | `hya-extra/model-fallback` | `tests/it/p07_bundle_policies_and_extras.rs` | Recovers pre-stream `unknown_model` via configured chain and fails cleanly when no chain matches |
| T2.29 | Bundle-declared HTTP APIs | `tests/it/p06_bundle_lifecycle_and_runtime.rs` | Session `session.usage` tree & single-session endpoints, global `POST`/`PUT`/`GET`/`DELETE`, `kind: bun` capability, `/v1/bundle-apis`, `bundle_api_*` error codes |
| T2.30 | `hya-extra/token-summary` | `tests/it/p07_bundle_policies_and_extras.rs` | Session-tree per-model usage API (`?scope=tree`, `?scope=session`, 400 `?scope=root`) and Markdown table tool |
| T2.31 | `wait` and `archive` lifecycle | `tests/it/p04_subagents_and_mailbox.rs` | Report & plain-answer finish wakes without re-steering; empty reply stalls; `archive` stops and `send` wakes |
| T2.32 | Multi-root Project permission scope | `tests/it/p02_permissions_and_questions.rs` | In-root reads never ask; outside reads ask concrete `<dir>/*`; allow-always is Project-scoped |
| T2.33 | `--yolo` bypasses `ExternalDirectory` | `tests/it/p02_permissions_and_questions.rs` | Outside-root read runs without raising any permission ask |
| T2.34 | `bash` cwd outside roots | `tests/it/p02_permissions_and_questions.rs` | Manual mode asks `Action::Bash` only, never `ExternalDirectory` |
| T2.35 | Secure relay golden path (`t=grpc`) | `tests/it/p38_relay.rs` | Full bridge path (health, Project, ask, SSE, PTY WS, RelayControl refusal, rotate, SIGTERM) + zero-plaintext capture hop |
| T2.36 | Secure relay golden path (`t=ws`) | `tests/it/p38_relay.rs` | T2.35 over pinned WebSocket binding + zero-plaintext capture hop |
| T2.37 | Secure relay over TLS | `tests/it/p38_relay.rs` | Private CA (`rcgen`), `hya://` link, prompt through bridge + zero-plaintext capture hop |
| T2.38 | Headless exec live subagent notice | `tests/it/p09_exec_headless.rs` | Early stop gets `HEADLESS RUN —` notice; lead waits, worker reports, `--include-subagents` captures child |
| T2.39 | Headless exec 2-notice cap (workers) | `tests/it/p09_exec_headless.rs` | Two ignored notices end the run promptly without waiting out a busy worker |
| T2.40 | Headless exec open todos | `tests/it/p09_exec_headless.rs` | Open todos alone do not continue the run |
| T2.41 | Headless exec announced next step | `tests/it/p09_exec_headless.rs` | Announced next step continues with the announcement quoted |
| T2.42 | Headless exec plain summary | `tests/it/p09_exec_headless.rs` | Plain summary after tool work ends run with no notice |
| T2.43 | Headless exec 2-notice cap (announcements) | `tests/it/p09_exec_headless.rs` | Two ignored announcement notices end the run |
| T2.44 | Headless exec `--time-budget` | `tests/it/p09_exec_headless.rs` | Opening names budget; `HEADLESS RUN — FINAL` appears once across tool results & continuation notices |
| T2.46 | Headless exec grace cutoff | `tests/it/p09_exec_headless.rs` | No continuation turn starts within grace |
| T2.47 | Headless exec spurious `<summary>` recovery | `tests/it/p09_exec_headless.rs` | Spurious `<summary>` triggers correction and resumes work |
| T2.48 | Headless exec DSML leak recovery | `tests/it/p09_exec_headless.rs` | Raw DSML tool-call leak triggers notice and recovers via structured tool call |
| T2.49 | Idle residents: exec stall vs server quiesce | `tests/it/p09_exec_headless.rs` (+ `p04_subagents_and_mailbox.rs`) | Exec reports stalled until archived (no `TEAM QUIESCED`); server team wakes lead with User `TEAM QUIESCED` |
| T2.50 | `tools.code.mode: only` | `tests/it/p07_bundle_policies_and_extras.rs` | Offers `code-mode__run` + kernel tools only; hidden tool docs inline; direct hidden call refused |
| T2.51 | `tools.code.mode: with_tools` | `tests/it/p07_bundle_policies_and_extras.rs` | Offers `code-mode__run` alongside usual tools; both nested and direct `read` succeed |
| T2.52 | Default `tools.code.mode` (`normal`) | `tests/it/p07_bundle_policies_and_extras.rs` | No `code-mode__run` offered; `hya bundle list` shows `hya/code-mode` disabled |
| T2.53 | Headless exec single-turn step fold | `tests/it/p09_exec_headless.rs` | 12 large `bash` steps against 120k window trigger kernel Rewrite removing `STEP_0_BEGIN` |
| T2.54 | Store robustness & exit durability | `tests/it/p43_store_robustness.rs` | Default PR smoke verifies clean JSONL/disk parity, clean archive drain, strict serve reject, and exec in-memory fallback; 7 `#[ignore = "nightly OS fault/soak"]` tests run in nightly |
| T2.55 | ACP v1 stdio | `tests/it/p03_mcp_and_custom_resources.rs` | Multiplexed sessions isolate same-name MCP tools, dispatch permissions and cancel. A second editor stays usable across disconnect. |

### Built-in tool coverage

`hya_tool_loader::builtins()` loads the four native tool families. The
[tool inventory](../architecture/agent-tool-surface.md) lists their registered
names and compatibility aliases. Track P runs `write`, `read`, `edit`, and `bash`
from the base family, `task`/`wait`/`archive`/`report` from the extended family,
`webfetch` from the network family, and `send`/`channels` from the channel
family. `todo` is a kernel tool with both bare and full-mode coverage in T1.15.

### Multi-agent scenarios need per-agent FakeLlm routing

`FakeLlm` holds one shared `VecDeque<ScriptStep>`, which is nondeterministic the
moment two agents are live: either can pop the other's step. `FakeLlm::route`
pins a queue to the agent whose **system prompt** contains a marker, and records
only that agent's request bodies. Attribution deliberately looks at `system`-role
content alone — a marker anywhere else in the transcript (tool-call arguments,
mail bodies) is echoed back into the *caller's* history too. With no routes
registered, dispatch is unchanged, so single-agent scenarios are unaffected.

Mail delivery has no HTTP surface, and only **resident** agents receive it
(`hya-engine::resident` injects a handle's unread inbox as `[mail from …]` user
prompts). The recipient's own next model request is therefore the only honest
delivery oracle — see `crates/hya-e2e/tests/it/p04_subagents_and_mailbox.rs`.

## Track T scenarios (moved to `packages/hya-tui-web`)

The legacy `T3.*` rows in `matrix.toml` (T3.1–T3.4) are marked retired because
they targeted the deleted legacy TypeScript TUI. Interactive TUI and WebUI E2E
coverage now lives in [`packages/hya-tui-web/e2e/`](../../packages/hya-tui-web/e2e)
and runs via `tui-exp` (Bun PTY + `@xterm/headless`) and Playwright in the
`tui` CI job; see [Browser-rendered TUI](../tui-web.md).

## Track I (index-only)

Deep engine / API coverage owned by in-process suites. Process E2E does not
replace these.

| ID | Title | Path |
| --- | --- | --- |
| I.nested | Nested spawn tree | `crates/hya-app/tests/it/nested_spawn_tree.rs` |
| I.subagent | Subagent/resident core | `crates/hya-engine/tests/it/subagent_lifecycle.rs` |
| I.v1_api | v1 HTTP contract | `crates/hya-server/tests/it/v1_api_core.rs` |
| I.v1_grpc_parity | v1 HTTP/gRPC parity | `crates/hya-server/tests/it/v1_grpc_parity.rs` |
| I.bundle_cli | Bundle CLI | `crates/hya-backend/tests/it/bundle_cli.rs` |

## The registry is enforced

`crates/hya-e2e/matrix.toml` is validated by
`cargo run -p xtask -- matrix-check`, which runs as a CI gate step. It fails on:

- a registered `path` that does not exist;
- a duplicate or malformed id;
- a Track P file that is registered but holds no test function;
- a Track P test file that **no** entry references (reverse drift);
- a numbering hole in a `T<major>` series that is neither used nor retired.

Correspondence is **file-level**: each consolidated golden-path module carries
multiple scenario IDs.

## Adding a scenario

1. Prefer extending Track I if the regression is pure engine/API shape.
2. For product-path regressions (config, real binary, FakeLlm tool loop), extend
   the matching consolidated module under `crates/hya-e2e/tests/it/` using
   `E2eEnvBuilder` (or `build_headless()` for `hya exec` without a server).
3. Register the ID in `matrix.toml` and this page.
4. Keep oracles honest — see [process-e2e.md](process-e2e.md#oracle-rules-do-not-weaken).
5. Run `cargo test -p hya-e2e --test it <module>:: -- --test-threads=4` and
   `cargo run -p xtask -- matrix-check` before landing.
