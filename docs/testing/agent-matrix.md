# Agent feature matrix

Process E2E lives in `crates/hya-e2e` (**Track P**): real `hya` +
scripted OpenAI-compatible FakeLlm, driven entirely through the `hya.v1`
contract. Existing in-process tests remain the authority for deep engine
semantics (**Track I**); they are indexed, not duplicated. There is no TUI
today; former Track T is retired (see below).

| Resource | Path |
| --- | --- |
| Machine registry | [`crates/hya-e2e/matrix.toml`](../../crates/hya-e2e/matrix.toml) |
| Harness guide | [process-e2e.md](process-e2e.md) |
| Testing overview | [README.md](README.md) |

## How to run

```sh
# Track P — process agent suite
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e -- --test-threads=1
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
| T1.9 | Skills load | `tests/p05_skills.rs` | `GET /v1/skills` lists skill; follow-up FakeLlm has body marker |
| T1.10 | MCP tool call | `tests/p06_mcp.rs` | `/mcp` connected; follow-up has `echo:…` result |
| T1.11 | Session list + resume | `tests/p07_session_lifecycle.rs` | `GET /v1/sessions` shows active sessions; second prompt on same id |
| T1.12 | Session transcript read | `tests/p12_context_api.rs` | Multi-turn user/assistant text in `GET /v1/sessions/{id}/messages` |
| T1.13 | Project AGENTS.md guidance | `tests/p13_project_agents_context.rs` | Compat-guided FakeLlm request contains AGENTS body marker |
| T1.14 | Compact / summarize | `tests/p14_compact_summarize.rs` | Compact injects summary into context; follow-up turn works |
| T1.15 | todowrite + edit | `tests/p15_todo_and_edit.rs` | Todo route lists item; edit rewrites file on disk |
| T1.16 | Custom slash catalog and route expansion | `tests/p18_custom_slash_resources.rs` | Supported command sources, precedence, exact single-pass expansion, route parity, and literal fallback |
| T1.17 | Skill-backed slash expansion | `tests/p18_custom_slash_resources.rs` | Direct Skill body admission, discovery order, no redundant `skill` call, and bootstrap cache boundaries |
| T1.18 | Custom command invokes builtin Skill Tool | `tests/p18_custom_slash_resources.rs` | Real `skill` result reaches the next model request; typed failures recover in the same Session |
| T1.19 | Custom command invokes plugin Tool | `tests/p18_custom_slash_resources.rs` | `remember` RPC result, permission, process death, respawn, drift rejection, and restart boundary |
| T1.20 | Custom command invokes MCP Tool | `tests/p18_custom_slash_resources.rs` | Namespaced result, structured faults, explicit reconnect, and old-binding pinning |
| T1.21 | Custom resource conflicts fail closed | `tests/p18_custom_slash_resources.rs` | Plugin/builtin/MCP collisions publish no partial runtime generation |
| T1.22 | Dynamic resource snapshots and reload | `tests/p18_custom_slash_resources.rs` | Skill and MCP next-Turn refresh, plugin restart boundary, and immutable old bindings |
| T1.23 | Structured custom Tool errors recover | `tests/p18_custom_slash_resources.rs` | One terminal Tool Event, structured replay, no replay execution, and later same-Session success |
| T2.1 | Subagent task | `tests/p08_subagent_task.rs` | Tree children ≥ 1, `general`, distinct child session |
| T2.2 | Nested tree depth≥2 | `tests/p09_nested_subagent.rs` | Depth ≥ 2, explore+plan, ≥ 3 session ids |
| T2.3 | Agent roster / roles | `tests/p10_agent_roster.rs` | `GET /v1/agents` lists build + spawnable roles |
| T2.4 | Swarm `roster` + `list_agents` | `tests/p16_swarm_mailbox.rs` | Caller's follow-up carries the teammate's handle, type, status and **real session id** |
| T2.5 | Swarm `send` (direct) | `tests/p16_swarm_mailbox.rs` | **Recipient's** next request contains `[mail from main/general-<operator>] …` |
| T2.6 | Swarm `send` (`#channel`) | `tests/p16_swarm_mailbox.rs` | Subscriber's next request contains the post; receipt reads `to #squad (1 recipient)` — the channel-branch count, not the direct-send constant |
| T2.7 | Hyabundle CLI lifecycle | `tests/p11_hyabundle.rs` | install/list/info/uninstall stdout |
| T2.8 | Hyabundle spawn agent | `tests/p11_hyabundle.rs` | Roster has package agent; events have scripted text |
| T2.9 | Swarm `channels` | `tests/p16_swarm_mailbox.rs` | Result reports `members:["general-<operator>"]` and `messages:1` the caller never supplied |
| T2.10 | Swarm `join` | `tests/p16_swarm_mailbox.rs` | Post-join message reaches the joiner; pre-join post never does |
| T2.11 | Swarm `leave` | `tests/p16_swarm_mailbox.rs` | Negative: departed member never sees the post, bounded by a still-subscribed member **and** a later direct ping it did see |
| T2.12 | Cross-unit `send` refused | `tests/p16_swarm_mailbox.rs` | Two units, two levels deep: sender's follow-up carries the scope refusal AND the payload never reaches the other unit ([ADR-0011](../adr/0011-hierarchy-scoped-mailbox.md)) |
| T2.13 | User-authored Workflow fan-out/fan-in | `tests/p17_workflow_composition.rs` | One discovered Workflow spawns four distinct stage Sessions, joins both parallel implementations into review, and returns the final report to the lead |
| T2.14 | Workflow Stage model routing and replay | `tests/p19_workflow_model_routing.rs` | Preferred 503 responses advance to the declared fallback with per-candidate effort; worker, verifier, and final route outcomes survive backend close/reopen without another provider request |
| T2.15 | model catalog discovery and offline fallback | `tests/p20_model_catalog_discovery.rs` | Explicit lists stay network-free and unwritten; empty lists rediscover anonymously each run without mutating config or a foreign OpenCode file; 401 and credentialed-forbidden catalogs surface `hya/offline` (exec prints the configuration explanation); mixed provider failure keeps the valid rows identical across CLI, `/v1/models`, `/v1/providers`, and `GET /v1/bootstrap` |
| T2.16 | [p21_agent_model_preference.rs](../../crates/hya-e2e/tests/p21_agent_model_preference.rs) | v1 agent-model preference set -> spawned `general` runs on the preferred model; listing reports `REMEMBERED` source | e2e (Track P) |
| T2.17 | [p22_dispatch_model_resolution.rs](../../crates/hya-e2e/tests/p22_dispatch_model_resolution.rs) | exact id override; substring fallback (bare vendor ids defer); user-config tier | e2e (Track P) |
| T2.18 | [p23_mcp_background.rs](../../crates/hya-e2e/tests/p23_mcp_background.rs) | long MCP call auto-backgrounds and steers reclaim | e2e (Track P) |
| T2.19 | [p24_bundle_schemas.rs](../../crates/hya-e2e/tests/p24_bundle_schemas.rs) | installed bundle `schemas:` / `extensions.process` / `resources.mcp` declarations surface through `bundle install`, `bundle schema <id>`, `bundle info`, and — after the first bound turn — `GET /v1/runtime/schemas` (owner = bundle source, canonical tool = the owning tool's stable id) | e2e (Track P) |
| T2.20 | [p25_goal_loop_bundle.rs](../../crates/hya-e2e/tests/p25_goal_loop_bundle.rs) | `hya/goal-loop` preset Skills and guide Agent; evaluator verdict reaches the next model request | e2e (Track P) |
| T2.21 | [p26_plugin_bundle.rs](../../crates/hya-e2e/tests/p26_plugin_bundle.rs) | agentless Plugin installs and publishes a static Skill | e2e (Track P) |
| T2.22 | [p27_bundle_process.rs](../../crates/hya-e2e/tests/p27_bundle_process.rs) | native process, JavaScript Plugin, and MCP execute from packaged files; private tools/hooks remain owner-scoped; uninstall removes new-session visibility | e2e (Track P) |
| T2.23 | [p28_subagent_bundle.rs](../../crates/hya-e2e/tests/p28_subagent_bundle.rs) | default subagent bundle worker mails its parent, reports, and archives | e2e (Track P) |
| T2.24 | [p29_claude_bundle.rs](../../crates/hya-e2e/tests/p29_claude_bundle.rs) | Claude directory imports into Plugin; packaged hook veto and Skill execute after source deletion; uninstall removes Skill | e2e (Track P) |
| T2.25 | [p30_channel_bundle.rs](../../crates/hya-e2e/tests/p30_channel_bundle.rs) | installed channel-only bundle denies child sends without parent delivery; uninstall restores default delivery in a new session | e2e (Track P) |
| T2.26 | [p31_extra_bundles.rs](../../crates/hya-e2e/tests/p31_extra_bundles.rs) | packaged `hya-extra/zvec-grep` Plugin bundle installs; a fake `zg` stdio MCP server on `PATH` answers `zvec_grep_search`, and the result reaches a full-plane `build` agent's follow-up request | e2e (Track P) |
| T2.27 | [p32_jev_model_router.rs](../../crates/hya-e2e/tests/p32_jev_model_router.rs) | packaged `hya-extra/jev-model-router` Bun process reads its bundle `config.yml`, asks a stub Jev endpoint once, and the provider request streams from the chosen tier model; a second turn in the same chain stays on it without another Jev call; a new chain is classified again; Jev HTTP 500 routes to `default_tier` (requires `bun` on `PATH`) | e2e (Track P) |
| T2.28 | [p33_model_fallback.rs](../../crates/hya-e2e/tests/p33_model_fallback.rs) | packaged `hya-extra/model-fallback` Bun process reads its bundle `config.yml`; a session pinned to an unrouted model recovers via the configured chain's `model.fallback` retry and the turn finishes; a session pinned to a model with no matching chain entry gives up and the turn fails instead of hanging (requires `bun` on `PATH`) | e2e (Track P) |
| T2.29 | [p34_bundle_apis.rs](../../crates/hya-e2e/tests/p34_bundle_apis.rs) | a packaged Plugin with a `kind: bun` `extensions.process` (a python3 fixture) declares tool `usage_probe` and five `apis:` endpoints; the tool call receives a capability (`context.describe` + `session.usage`); `GET /v1/bundle-apis` lists the endpoints with their JSON Schemas; after a turn that spawns a `general` subagent with FakeLlm reporting deterministic usage, the session endpoint `GET /v1/sessions/{session}/bundles/acme%2Fusage-apis/usage` answers the process's own JSON with one row per session of the tree plus a merged total whose rounds equal the FakeLlm calls; `?scope=session` narrows to one row and `?scope=root` is the process's own 400; a global `POST /v1/bundles/acme%2Fusage-apis/api/echo/a%20b/7` reaches the process with body, decoded path params, and query, answers 201, and its capability has no session (`session.usage` → `-32001`); a global `PUT`/`GET`/`DELETE` trio over `/items/{key}` keeps process-owned state (201/200/200/204/404); unknown endpoint/bundle/session answer `bundle_api_not_found`/`session_not_found`, a wrong method `bundle_api_method_not_allowed`, a non-JSON body `bundle_api_bad_request` | e2e (Track P) |
| T2.30 | [p35_token_summary.rs](../../crates/hya-e2e/tests/p35_token_summary.rs) | packaged `hya-extra/token-summary` Bun process bundle; after a turn that spawns a subagent via `task` with FakeLlm reporting deterministic (known-split) usage, `GET /v1/sessions/{session}/bundles/hya-extra%2Ftoken-summary/usage` returns (as the bundle's own JSON body) per-model `input`/`cache_creation`/`cache_read`/`output`/`thinking`/`visible_output`/`rounds` matching the reported usage across both sessions of the tree, with the grand total equal to the one model used; `?scope=session` narrows to the root's own rounds and `?scope=root` is the bundle's own 400; a second turn where `build` calls tool `token-summary__token_summary` and the follow-up model request carries the rendered Markdown table | e2e (Track P) |
| T2.31 | [p36_wait_archive.rs](../../crates/hya-e2e/tests/p36_wait_archive.rs) | the lead spawns `hya-worker` and calls `wait`: the worker's `report` wakes the wait inside the lead's own turn (`Subagents finished.` plus the report reach the lead's follow-up request); a repeated `wait` on that worker returns at once as `nothing_to_wait_for` with the report under `already_finished`, and the report mail is never steered again (no `[NEW MAIL · …]` notice); a second run's worker ends its turn without a report, which wakes `wait` as `stalled` (never finished), then the lead `archive`s it (`Archived \`main/hya-worker-<operator>\``; the backend runs with `HYA_HANDLE_SEED` so the script knows the handle) and `send` to its handle wakes it again (the worker's next request carries the mail) | e2e (Track P) |

### Built-in tool coverage

`ToolRegistry::builtins()` advertises exactly **27** canonical Tool names. The
dispatch-only aliases `shell`, `fetch`, `search`, `todo`, `patch`, and `plan` are hidden
from provider schemas and are not counted as canonical Tools. Track P directly
exercises **15** canonical Tools:

| Covered (15) | Not directly covered by Track P (12) |
| --- | --- |
| `read`, `write`, `edit`, `bash`, `question`, `skill`, `task`, `todowrite`, `send`, `roster`, `channels`, `join`, `leave`, `list_agents`, `workflow` | `ls`, `glob`, `find`, `grep`, `lsp`, `ask_user`, `apply_patch`, `webfetch`, `websearch`, `plan_exit`, `invalid`, `announce` |

### Multi-agent scenarios need per-agent FakeLlm routing

`FakeLlm` holds one shared `VecDeque<ScriptStep>`, which is nondeterministic the
moment two agents are live: either can pop the other's step. `FakeLlm::route`
pins a queue to the agent whose **system prompt** contains a marker, and records
only that agent's request bodies. Attribution deliberately looks at `system`-role
content alone — a marker anywhere else in the transcript (tool-call arguments,
mail bodies) is echoed back into the *caller's* history too. With no routes
registered, dispatch is unchanged, so single-agent scenarios are unaffected.

Mail delivery has no HTTP surface, and only **resident** agents receive it
(`hya-core::resident` injects a handle's unread inbox as `[mail from …]` user
prompts). The recipient's own next model request is therefore the only honest
delivery oracle — see the module docs of `tests/p16_swarm_mailbox.rs` for the
ordering rules that keep those scenarios deterministic.

## Track T scenarios (retired)

Track T has no live scenarios. The whole T3 series is retired in
`matrix.toml` (T3.1 permission reply, T3.2 multi-agent task presentation,
T3.3 agent roster, T3.4 PTY smoke): the real-backend trio verified the deleted
Compat HTTP surface, and the legacy TypeScript TUI package was then removed
from the repository entirely. Frontend-on-`hya-sdk-v1` scenarios return to the
matrix only when a future TUI is built; register them under the T3 series then,
following the ID allocation rule below.

### PTY policy and recorded timeout

The old PTY suites (`pty-smoke.test.ts`, `workflow-pty.test.ts`) ran in the
non-gating Bun step and were removed with the old TUI's backend integration.

The retained historical observation is run `31053432077` on commit `fee38938`:
the stable test `Linux PTY renders home, opens a session, and restores the
terminal` timed out at its root-draft presentation oracle. Byte-identical code
passed the same step in two other CI runs and passed 3/3 locally. No root cause
was established. Read a new red PTY-smoke result as a regression only after
checking whether it is the same presentation timeout.

### Historical intermittent-failure observations (2026-08-06)

The following table records the observations known on 2026-08-06. It is not a
claim that these tests still fail at the same rate on current HEAD. Stable test
symbols replace volatile source-line anchors.

| Test | Recorded symptom | Historical evidence |
| --- | --- | --- |
| `transient_sidecar_loss_interrupts_running_member_before_provider_release` (`crates/hya-core/tests/subagent.rs`) | assertion failure | **1 CI observation** (run `31061204771`, on `573924f4`); 3 later CI runs on `main` green |
| `foreign_promotion_is_wake_only` (`crates/hya-app/src/runtime.rs`) | `Elapsed(())` on a store-state poll | 4/30 runs, **only under artificial 24× CPU saturation**; never observed on CI |
| `captures_code_and_state_from_callback` (`crates/hya-app/src/oauth/callback.rs`) | `ConnectionRefused` | 3/60 runs under the same artificial load; never observed on CI |

At the time of the record:

- The first was the only failure with CI evidence. It fired on the commit
  preceding the hardening round and did not recur in three subsequent runs.
  Nothing in that round touched `hya-core`'s subagent tests, so the later green
  runs did not prove a fix.
- The second busy-spun on `tokio::task::yield_now()` while polling the store,
  starving the promotion task under 24× oversubscription. That probe behavior
  was not evidence of a CI-relevant defect.
- The third had a rebind race by inspection: the test bound a port, dropped the
  listener, spawned a thread to re-bind, and slept before connecting. The
  durable fix would pass an already-bound `TcpListener` into
  `wait_for_callback`, not add a longer sleep or retry.

## Track I (index-only)

Deep engine / API coverage owned by in-process suites. Process E2E does not
replace these.

| ID | Title | Path |
| --- | --- | --- |
| I.nested | Nested spawn tree | `crates/hya-app/tests/nested_spawn_tree.rs` |
| I.subagent | Subagent/resident core | `crates/hya-core/tests/subagent.rs` |
| I.v1_api | v1 HTTP contract | `crates/hya-server/tests/v1_api.rs` |
| I.v1_grpc_parity | v1 HTTP/gRPC parity | `crates/hya-server/tests/v1_grpc_parity.rs` |
| I.bundle_cli | Bundle CLI | `crates/hya-backend/tests/bundle_cli.rs` |
| I.compact_engine | Engine compact_context | `crates/hya-core/tests/compact_context.rs` |

The previous Compat-route index rows (permission/question, MCP, session
context, compact APIs under `crates/hya-server/tests/compat_*.rs`) were removed
with the deleted surface; `v1_api.rs` and `v1_grpc_parity.rs` own that coverage
for the `hya.v1` contract.

## The registry is enforced

`crates/hya-e2e/matrix.toml` is validated by
`cargo run -p xtask -- matrix-check` (there is **no** Cargo alias named
`xtask` in this workspace), which runs as a CI gate step. It fails on:

- a registered `path` that does not exist;
- a duplicate or malformed id;
- a Track P file that is registered but holds no test function;
- a Track P test file that **no** entry references (reverse drift — an
  unregistered scenario is as much a registry failure as a phantom row);
- a numbering hole in a `T<major>` series that is neither used nor retired.

Bidirectional drift is enforced for Track P only. Track I rows are index
pointers into other crates that are deliberately not one-to-one with registry
rows; checking those would generate false failures.

Correspondence is **file-level**: `p01` carries two ids in one function, `p02`
carries three, `p03` has one id and two functions.

### ID allocation rule

- A new scenario takes the next free number in its series.
- Retiring an id requires a `[[retired]]` entry with a real reason — not
  "unused". If the original intent is unrecoverable, say that.
- Both rules are enforced by `matrix-check`, so a gap cannot reappear silently.
  `T1.1` and `T1.6` are retired on exactly those grounds: nothing in this
  repository's history records what they were meant to cover.

## Adding a scenario

1. Prefer extending Track I if the regression is pure engine/API shape.
2. For product-path regressions (config, real binary, FakeLlm tool loop), add
   `crates/hya-e2e/tests/pNN_*.rs` using `E2eEnvBuilder`.
3. Register the ID in `matrix.toml` and this page.
4. Keep oracles honest — see [process-e2e.md](process-e2e.md#oracle-rules-do-not-weaken).
5. Run `cargo test -p hya-e2e --test pNN_… -- --test-threads=1` then the full
   crate suite before landing.
